//! Space Invaders, played by a robot gunner.
//!
//! Nine columns is exactly the width of a rank of invaders, which is why this
//! game fits a panel that defeats most others: the shape of the screen is the
//! shape of the game. Aliens are a pixel each — at this size a sprite would be
//! a smudge — spaced two apart so the rank reads as a rank and has somewhere to
//! march.
//!
//! The gunner is not a perfect one. It leads its shots, dodges bombs it can see
//! coming, and misses when a column dies underneath a bullet already in flight,
//! so a wave is a fight rather than a demolition.

use std::time::Duration;

use rand::RngExt;
use rand::rngs::StdRng;

use crate::canvas::{self, Canvas};
use crate::device::ColorMode;
use crate::scene::{Area, Scene, rng_from};

/// Aliens across and down, and the gap between them.
const COLUMNS: usize = 4;
const ROWS: usize = 3;
const SPACING: i32 = 2;
/// Columns the rank occupies: a pixel each, with the gaps between them.
///
/// Written out because the counts are `usize` and the panel is `i32`; a test
/// keeps the two in step.
const RANK_WIDTH: i32 = 7;

/// Where the rank starts: centred, so it has the same room either way. A rank
/// as wide as the panel could not march at all, which a test guards against.
const START_X: i32 = (canvas::WIDTH - RANK_WIDTH) / 2;
const START_Y: i32 = 2;
// A rank as wide as the panel could not march at all.
const _: () = assert!(START_X > 0 && RANK_WIDTH < canvas::WIDTH);
/// How much lower each wave begins, up to a limit.
const WAVE_DROP: i32 = 1;
const MAX_WAVE_DROP: i32 = 4;

/// The gunner's rows: the muzzle, then the base.
const MUZZLE_ROW: i32 = canvas::HEIGHT - 2;
const BASE_ROW: i32 = canvas::HEIGHT - 1;
/// The base is three wide, so one pixel either side of its centre.
const BASE_HALF: i32 = 1;

/// Seconds between alien steps with a full rank, and with one left.
///
/// The rank speeding up as it thins is the whole rhythm of the game: the last
/// alien is a sprint, not a formality.
const STEP_SLOW: f32 = 0.50;
const STEP_FAST: f32 = 0.09;

/// Seconds a bullet and a bomb take to cross one row.
const BULLET_STEP: f32 = 0.035;
const BOMB_STEP: f32 = 0.13;
/// Seconds the gunner takes to slide one column.
const CANNON_STEP: f32 = 0.07;

/// Chance that the rank drops a bomb on any one of its steps.
const BOMB_CHANCE: f32 = 0.22;
/// Bombs in the air at once. Three of them on a panel nine wide leaves the
/// gunner nowhere to stand, and a game it cannot win is not one to watch.
const MAX_BOMBS: usize = 2;

/// How far above the gunner a bomb starts counting as a reason to move.
const DODGE_ROWS: i32 = 12;

/// Flashes a second while the loss is held.
const FLASH_RATE: f32 = 6.0;

/// Seconds the end of a wave and the loss of the gunner are held.
const CLEARED_DELAY: f32 = 0.8;
const HIT_DELAY: f32 = 1.1;
/// Longest simulated step, so a stalled thread does not teleport anything.
const MAX_STEP: f32 = 0.1;

/// Brightness of a bomb under the alien that dropped it, in greyscale.
const BOMB_LEVEL: u8 = 120;

/// What the game is doing.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Playing,
    /// The rank is down; the panel holds still before the next wave.
    Cleared(f32),
    /// The gunner is gone; everything flashes before starting over.
    Hit(f32),
}

/// A shot travelling up, or a bomb travelling down.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Shot {
    x: i32,
    y: i32,
}

/// Space Invaders.
pub struct Invaders {
    mode: ColorMode,
    rng: StdRng,
    /// Which aliens are still alive, by row and column.
    alive: [[bool; COLUMNS]; ROWS],
    /// Top-left of the rank, in pixels.
    rank_x: i32,
    rank_y: i32,
    marching_right: bool,
    cannon: i32,
    bullet: Option<Shot>,
    bombs: Vec<Shot>,
    phase: Phase,
    wave: u32,
    step_timer: f32,
    bullet_timer: f32,
    bomb_timer: f32,
    cannon_timer: f32,
}

impl Invaders {
    /// Starts a game.
    #[must_use]
    pub fn new(seed: Option<u64>, mode: ColorMode) -> Self {
        let mut game = Self {
            mode,
            rng: rng_from(seed),
            alive: [[true; COLUMNS]; ROWS],
            rank_x: START_X,
            rank_y: START_Y,
            marching_right: true,
            cannon: canvas::WIDTH / 2,
            bullet: None,
            bombs: Vec::new(),
            phase: Phase::Playing,
            wave: 0,
            step_timer: 0.0,
            bullet_timer: 0.0,
            bomb_timer: 0.0,
            cannon_timer: 0.0,
        };
        game.start_wave();
        game
    }

    /// Sets a fresh rank up, a little lower each wave.
    fn start_wave(&mut self) {
        self.alive = [[true; COLUMNS]; ROWS];
        self.rank_x = START_X;
        let drop = i32::try_from(self.wave).unwrap_or(0) * WAVE_DROP;
        self.rank_y = START_Y + drop.min(MAX_WAVE_DROP);
        self.marching_right = true;
        self.bullet = None;
        self.bombs.clear();
        self.step_timer = 0.0;
    }

    /// Back to the first wave, gunner in the middle.
    fn restart(&mut self) {
        self.wave = 0;
        self.cannon = canvas::WIDTH / 2;
        self.start_wave();
    }

    /// Where an alien sits, whether or not it is alive.
    fn alien_at(&self, row: usize, column: usize) -> (i32, i32) {
        let step = |index: usize| i32::try_from(index).unwrap_or(0) * SPACING;
        (self.rank_x + step(column), self.rank_y + step(row))
    }

    /// Every living alien, as a position.
    fn living(&self) -> impl Iterator<Item = (i32, i32)> + '_ {
        (0..ROWS).flat_map(move |row| {
            (0..COLUMNS)
                .filter(move |column| self.alive[row][*column])
                .map(move |column| self.alien_at(row, column))
        })
    }

    /// How many are left.
    fn remaining(&self) -> usize {
        self.alive
            .iter()
            .flat_map(|row| row.iter())
            .filter(|alive| **alive)
            .count()
    }

    /// The columns the rank actually occupies now.
    ///
    /// Dead outer columns are not part of it: as the rank is eaten away from
    /// the edges the survivors get more room, which is what makes the last few
    /// aliens sweep the whole width.
    fn extent(&self) -> Option<(i32, i32)> {
        let mut left = i32::MAX;
        let mut right = i32::MIN;
        for (x, _) in self.living() {
            left = left.min(x);
            right = right.max(x);
        }
        (left <= right).then_some((left, right))
    }

    /// Seconds between steps, from a full rank down to the last one.
    fn step_interval(&self) -> f32 {
        let count = |value: usize| f32::from(u16::try_from(value).unwrap_or(u16::MAX));
        let total = count(ROWS * COLUMNS);
        let left = count(self.remaining());
        let share = ((left - 1.0) / (total - 1.0)).clamp(0.0, 1.0);
        STEP_FAST + (STEP_SLOW - STEP_FAST) * share
    }

    /// Marches the rank sideways, or down and back the other way at an edge.
    fn step_rank(&mut self) {
        let Some((left, right)) = self.extent() else {
            return;
        };

        let blocked = if self.marching_right {
            right >= canvas::WIDTH - 1
        } else {
            left <= 0
        };

        if blocked {
            self.marching_right = !self.marching_right;
            self.rank_y += 1;
        } else if self.marching_right {
            self.rank_x += 1;
        } else {
            self.rank_x -= 1;
        }

        self.maybe_drop_bomb();
    }

    /// Drops a bomb from the lowest alien of a random living column.
    fn maybe_drop_bomb(&mut self) {
        if self.bombs.len() >= MAX_BOMBS || self.rng.random::<f32>() >= BOMB_CHANCE {
            return;
        }

        // The lowest alien in each column is the only one with a clear shot.
        let mut lowest: Vec<(i32, i32)> = Vec::new();
        for column in 0..COLUMNS {
            let found = (0..ROWS)
                .rev()
                .find(|row| self.alive[*row][column])
                .map(|row| self.alien_at(row, column));
            if let Some(position) = found {
                lowest.push(position);
            }
        }
        if lowest.is_empty() {
            return;
        }

        let pick = self.rng.random_range(0..lowest.len());
        let (x, y) = lowest[pick];
        self.bombs.push(Shot { x, y: y + 1 });
    }

    /// Where an alien will be by the time a shot fired now could reach it.
    ///
    /// A bullet takes the better part of a second to climb the panel, and the
    /// rank walks a pixel every step while it climbs. Aiming at where an alien
    /// is means arriving two columns behind it, and a wave that never falls —
    /// which is exactly what happened before this existed.
    /// Alien steps that fit into a bullet's climb from the muzzle to row `y`.
    fn steps_while_a_shot_climbs(&self, y: i32) -> i32 {
        let rows = f32::from(u8::try_from((MUZZLE_ROW - 1 - y).max(0)).unwrap_or(0));
        canvas::floor_pixel(rows * BULLET_STEP / self.step_interval())
    }

    fn predicted_x(&self, x: i32, y: i32) -> i32 {
        let steps = self.steps_while_a_shot_climbs(y);
        let Some((left, right)) = self.extent() else {
            return x;
        };

        // The rank turns at the walls, and a thin rank turns often: the last
        // alien crosses the panel and comes back inside one flight. Walking the
        // march rather than extrapolating it is what makes the endgame end.
        let (low, high) = (x - left, x + (canvas::WIDTH - 1 - right));
        let mut position = x;
        let mut direction = if self.marching_right { 1 } else { -1 };
        for _ in 0..steps {
            let next = position + direction;
            if next < low || next > high {
                // A blocked step turns and descends instead of moving.
                direction = -direction;
            } else {
                position = next;
            }
        }
        position
    }

    /// The column the gunner wants to be in.
    ///
    /// Safety first, then aim: it picks the safe column nearest the shot it
    /// wants to take. Stepping one pixel aside instead — the obvious thing —
    /// walked straight back under the bomb on the following tick, and the
    /// gunner spent its afternoons being hit by the same bomb twice.
    fn wanted_column(&self) -> i32 {
        let aim = self
            .living()
            .min_by_key(|(x, y)| (-y, (x - self.cannon).abs()))
            .map_or(self.cannon, |(x, y)| {
                self.predicted_x(x, y).clamp(0, canvas::WIDTH - 1)
            });

        (0..canvas::WIDTH)
            .filter(|column| self.threat_to(*column).is_none())
            .min_by_key(|column| ((column - aim).abs(), (column - self.cannon).abs()))
            .unwrap_or_else(|| {
                // Nowhere is safe: stand where the bomb is furthest away.
                (0..canvas::WIDTH)
                    .max_by_key(|column| self.threat_to(*column).unwrap_or(i32::MAX))
                    .unwrap_or(self.cannon)
            })
    }

    /// How many rows away the soonest bomb aimed at `column` is.
    fn threat_to(&self, column: i32) -> Option<i32> {
        self.bombs
            .iter()
            .filter(|bomb| (bomb.x - column).abs() <= BASE_HALF)
            .map(|bomb| BASE_ROW - bomb.y)
            .filter(|rows| *rows <= DODGE_ROWS)
            .min()
    }

    /// Fires if the gunner is lined up and has nothing in the air.
    fn maybe_fire(&mut self) {
        if self.bullet.is_some() {
            return;
        }
        let lined_up = self
            .living()
            .any(|(x, y)| self.predicted_x(x, y) == self.cannon);
        if lined_up {
            self.bullet = Some(Shot {
                x: self.cannon,
                y: MUZZLE_ROW - 1,
            });
        }
    }

    /// Moves the bullet up, taking an alien with it if it meets one.
    fn step_bullet(&mut self) {
        let Some(mut bullet) = self.bullet else {
            return;
        };
        bullet.y -= 1;
        if bullet.y < 0 {
            self.bullet = None;
            return;
        }

        for row in 0..ROWS {
            for column in 0..COLUMNS {
                if self.alive[row][column] && self.alien_at(row, column) == (bullet.x, bullet.y) {
                    self.alive[row][column] = false;
                    self.bullet = None;
                    return;
                }
            }
        }
        self.bullet = Some(bullet);
    }

    /// Moves every bomb down, and reports whether one reached the gunner.
    fn step_bombs(&mut self) -> bool {
        let mut hit = false;
        for bomb in &mut self.bombs {
            bomb.y += 1;
            if bomb.y >= MUZZLE_ROW && (bomb.x - self.cannon).abs() <= BASE_HALF {
                hit = true;
            }
        }
        self.bombs.retain(|bomb| bomb.y < canvas::HEIGHT);
        hit
    }

    /// Whether the rank has walked into the gunner.
    fn rank_has_landed(&self) -> bool {
        self.living().any(|(_, y)| y >= MUZZLE_ROW)
    }

    /// Draws the rank, the gunner and everything in the air.
    fn draw_game(&self, canvas: &mut Canvas, area: Area) {
        for (x, y) in self.living() {
            canvas.set_max(x, area.row(y), u8::MAX);
        }

        let bomb_level = if self.mode == ColorMode::Bw {
            u8::MAX
        } else {
            BOMB_LEVEL
        };
        for bomb in &self.bombs {
            canvas.set_max(bomb.x, area.row(bomb.y), bomb_level);
        }
        if let Some(bullet) = self.bullet {
            canvas.set_max(bullet.x, area.row(bullet.y), u8::MAX);
        }

        canvas.hline(
            self.cannon - BASE_HALF,
            self.cannon + BASE_HALF,
            area.row(BASE_ROW),
            u8::MAX,
        );
        canvas.set_max(self.cannon, area.row(MUZZLE_ROW), u8::MAX);
    }
}

impl Scene for Invaders {
    fn name(&self) -> &'static str {
        "invaders"
    }

    /// The whole panel: a rank that cannot march is not the game.
    fn min_height(&self) -> i32 {
        canvas::HEIGHT
    }

    fn update(&mut self, delta: Duration) {
        let dt = delta.as_secs_f32().min(MAX_STEP);

        match self.phase {
            Phase::Cleared(remaining) => {
                let remaining = remaining - dt;
                self.phase = if remaining <= 0.0 {
                    self.wave += 1;
                    self.start_wave();
                    Phase::Playing
                } else {
                    Phase::Cleared(remaining)
                };
                return;
            }
            Phase::Hit(remaining) => {
                let remaining = remaining - dt;
                self.phase = if remaining <= 0.0 {
                    self.restart();
                    Phase::Playing
                } else {
                    Phase::Hit(remaining)
                };
                return;
            }
            Phase::Playing => {}
        }

        self.cannon_timer += dt;
        while self.cannon_timer >= CANNON_STEP {
            self.cannon_timer -= CANNON_STEP;
            let wanted = self.wanted_column();
            self.cannon += (wanted - self.cannon).signum();
            self.cannon = self.cannon.clamp(0, canvas::WIDTH - 1);
            self.maybe_fire();
        }

        self.bullet_timer += dt;
        while self.bullet_timer >= BULLET_STEP {
            self.bullet_timer -= BULLET_STEP;
            self.step_bullet();
        }

        self.bomb_timer += dt;
        while self.bomb_timer >= BOMB_STEP {
            self.bomb_timer -= BOMB_STEP;
            if self.step_bombs() {
                self.phase = Phase::Hit(HIT_DELAY);
                return;
            }
        }

        self.step_timer += dt;
        while self.step_timer >= self.step_interval() {
            self.step_timer -= self.step_interval();
            self.step_rank();
        }

        if self.remaining() == 0 {
            self.phase = Phase::Cleared(CLEARED_DELAY);
        } else if self.rank_has_landed() {
            self.phase = Phase::Hit(HIT_DELAY);
        }
    }

    fn render(&self, canvas: &mut Canvas, area: Area) {
        match self.phase {
            Phase::Hit(remaining) => {
                // Flashing says the gunner lost without needing a word for it.
                if canvas::floor_pixel(remaining * FLASH_RATE) % 2 == 0 {
                    for y in 0..area.height {
                        canvas.hline(0, canvas::WIDTH - 1, area.row(y), u8::MAX);
                    }
                } else {
                    self.draw_game(canvas, area);
                }
            }
            Phase::Cleared(_) | Phase::Playing => self.draw_game(canvas, area),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BASE_HALF, COLUMNS, Invaders, MUZZLE_ROW, Phase, ROWS, Shot};
    use crate::canvas::{self, Canvas};
    use crate::device::ColorMode;
    use crate::scene::{Area, Scene};
    use std::time::Duration;

    /// A game with a fixed seed, so a run is the same run every time.
    fn game() -> Invaders {
        Invaders::new(Some(7), ColorMode::Bw)
    }

    /// Runs the game for a while, a frame at a time.
    fn play(game: &mut Invaders, seconds: f32) {
        let frame = Duration::from_millis(33);
        let steps = crate::canvas::floor_pixel(seconds / frame.as_secs_f32());
        for _ in 0..steps {
            game.update(frame);
        }
    }

    #[test]
    fn the_written_down_width_matches_the_rank() {
        // The panel counts in `i32` and the rank in `usize`, so the width is
        // written out; that it fits with room to march is asserted at compile
        // time, and this is what keeps the number honest.
        let across = i32::try_from(COLUMNS - 1).expect("four columns");
        assert_eq!(
            super::RANK_WIDTH,
            across * super::SPACING + 1,
            "the written-down width drifted from the rank"
        );
    }

    #[test]
    fn nothing_ever_leaves_the_panel() {
        let mut game = game();
        for _ in 0..3000 {
            game.update(Duration::from_millis(33));
            assert!(
                (0..canvas::WIDTH).contains(&game.cannon),
                "the gunner walked off at {}",
                game.cannon
            );
            for (x, y) in game.living() {
                assert!((0..canvas::WIDTH).contains(&x), "an alien is at x={x}");
                assert!(y < canvas::HEIGHT, "an alien is at y={y}");
            }
            for bomb in &game.bombs {
                assert!((0..canvas::WIDTH).contains(&bomb.x), "a bomb is off-panel");
            }
            if let Some(bullet) = game.bullet {
                assert!(bullet.y >= 0, "a bullet went through the ceiling");
            }
        }
    }

    #[test]
    fn nothing_is_drawn_outside_the_area() {
        let mut game = game();
        play(&mut game, 4.0);
        for top in [0, 0] {
            let area = Area {
                top,
                height: canvas::HEIGHT,
            };
            let mut canvas = Canvas::new();
            game.render(&mut canvas, area);
            for y in 0..canvas::HEIGHT {
                if y < area.top || y >= area.top + area.height {
                    for x in 0..canvas::WIDTH {
                        assert_eq!(canvas.get(x, y), 0, "drew at {x},{y}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_gunner_clears_a_wave_rather_than_stalling() {
        // A gunner that never finishes would show the same rank until the
        // laptop is closed. It is allowed to lose on the way — it does, about
        // half the time — but a minute has to see a rank fall.
        let mut game = game();
        let mut cleared = false;
        for _ in 0..1800 {
            game.update(Duration::from_millis(33));
            if matches!(game.phase, Phase::Cleared(_)) || game.wave > 0 {
                cleared = true;
                break;
            }
        }
        assert!(cleared, "no rank fell in a minute of play");
    }

    #[test]
    fn a_shot_lands_where_it_was_led() {
        // Checked against the march itself rather than against a formula: the
        // lead has to survive the rank turning at a wall, which is most of what
        // it does once only a few aliens are left.
        for survivors in [12, 3, 1] {
            let mut game = game();
            let mut left = 12;
            for row in 0..ROWS {
                for column in 0..COLUMNS {
                    if left > survivors {
                        game.alive[row][column] = false;
                        left -= 1;
                    }
                }
            }

            let (x, y) = game.living().next().expect("someone is left");
            let predicted = game.predicted_x(x, y);
            let steps = game.steps_while_a_shot_climbs(y);
            for _ in 0..steps {
                game.step_rank();
            }
            let (actual, _) = game.living().next().expect("still alive");

            assert_eq!(
                predicted, actual,
                "with {survivors} left, the shot was led to {predicted} and the alien was at {actual}"
            );
        }
    }

    #[test]
    fn the_rank_speeds_up_as_it_thins() {
        // The rhythm of the game: a full rank shuffles, the last one sprints.
        let mut game = game();
        let full = game.step_interval();
        for row in 0..ROWS {
            for column in 0..COLUMNS {
                game.alive[row][column] = false;
            }
        }
        game.alive[0][0] = true;
        assert!(
            game.step_interval() < full,
            "one alien marches no faster than twelve"
        );
    }

    #[test]
    fn a_thinned_rank_gets_the_room_its_dead_leave_behind() {
        // Edge detection follows the living, so survivors sweep wider as the
        // outer columns go — otherwise the last alien would rattle around in
        // the box the first rank happened to occupy.
        let mut game = game();
        let (left, right) = game.extent().expect("a fresh rank is alive");
        for row in 0..ROWS {
            game.alive[row][0] = false;
        }
        let (thinned_left, _) = game.extent().expect("still alive");
        assert!(thinned_left > left, "the dead column still counted");
        assert_eq!(right, game.extent().expect("still alive").1);
    }

    #[test]
    fn a_bomb_reaching_the_gunner_is_a_hit_and_one_beside_it_is_not() {
        // The collision rule on its own: driving this through `update` tests
        // the dodge instead, and the dodge is good enough to get out of the
        // way, which is a different thing worth a different test.
        let mut game = game();
        game.bombs = vec![Shot {
            x: game.cannon,
            y: MUZZLE_ROW - 1,
        }];
        assert!(game.step_bombs(), "the gunner shrugged off a direct hit");

        let mut beside = self::game();
        beside.bombs = vec![Shot {
            x: beside.cannon + BASE_HALF + 1,
            y: MUZZLE_ROW - 1,
        }];
        assert!(!beside.step_bombs(), "a near miss counted as a hit");
    }

    #[test]
    fn the_gunner_steps_out_from_under_a_bomb() {
        let mut game = game();
        game.cannon = 4;
        game.bombs = vec![Shot {
            x: 4,
            y: super::BASE_ROW - 4,
        }];
        let wanted = game.wanted_column();
        assert!(
            (wanted - 4).abs() > super::BASE_HALF,
            "it moved to {wanted}, still under the bomb at 4"
        );
    }

    #[test]
    fn a_landing_rank_ends_the_run() {
        let mut game = game();
        game.rank_y = MUZZLE_ROW;
        game.update(Duration::from_millis(33));
        assert!(
            matches!(game.phase, Phase::Hit(_)),
            "the aliens walked over the gunner without ending it"
        );
    }

    #[test]
    fn the_gunner_is_always_drawn() {
        // Whatever else is happening, the panel has to show who is playing.
        let mut game = game();
        for _ in 0..40 {
            play(&mut game, 0.3);
            if matches!(game.phase, Phase::Hit(_)) {
                continue;
            }
            let mut canvas = Canvas::new();
            game.render(&mut canvas, Area::FULL);
            let base = (0..canvas::WIDTH)
                .filter(|x| canvas.get(*x, canvas::HEIGHT - 1) > 0)
                .count();
            assert!(base > 0, "the gunner vanished");
        }
    }
}
