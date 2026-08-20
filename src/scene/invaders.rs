//! Space Invaders, played by a robot gunner.
//!
//! Nine columns is exactly the width of a rank of invaders, which is why this
//! game fits a panel that defeats most others: the shape of the screen is the
//! shape of the game. Aliens are a pixel each — at this size a sprite would be
//! a smudge — spaced apart so the rank reads as a rank and has somewhere to
//! march.
//!
//! The gunner is not a perfect one. It leads its shots, shelters behind the
//! bunkers when something is falling, and loses a life often enough that a run
//! is a fight rather than a demolition.

use std::time::Duration;

use rand::RngExt;
use rand::rngs::StdRng;

use crate::canvas::{self, Canvas};
use crate::device::ColorMode;
use crate::scene::{Area, Scene, rng_from};

/// The largest rank a wave can field.
///
/// Three columns, because four cannot be fitted with room left to march: at a
/// gap of two they span seven of the nine columns, and a rank with two columns
/// of travel turns at a wall every other step and descends with every turn.
const MAX_COLUMNS: usize = 3;
const MAX_ROWS: usize = 5;

/// One wave's rank: columns, rows, and the gap between aliens.
///
/// Waves cycle through these. A rank as wide as the panel bounces off a wall
/// every other step and descends with it, which is what made the first version
/// land on the gunner inside half a minute whatever it did; the narrow ones
/// have room to march and take their time coming down.
struct Formation {
    /// Aliens across and down. Kept as panel coordinates rather than counts,
    /// so the geometry never has to cross between `usize` and `i32`.
    columns: i32,
    rows: i32,
    spacing: i32,
}

const FORMATIONS: [Formation; 4] = [
    // Twelve aliens in a block.
    Formation {
        columns: 3,
        rows: 4,
        spacing: 2,
    },
    // Two wide columns that sweep most of the panel.
    Formation {
        columns: 2,
        rows: 5,
        spacing: 3,
    },
    // A smaller block, which thins into a sprint sooner.
    Formation {
        columns: 3,
        rows: 3,
        spacing: 2,
    },
    // A narrow pair with the run of the whole width.
    Formation {
        columns: 2,
        rows: 4,
        spacing: 2,
    },
];

impl Formation {
    /// Columns the rank occupies: a pixel each, with the gaps between them.
    const fn width(&self) -> i32 {
        (self.columns - 1) * self.spacing + 1
    }

    /// The same counts, for indexing the rank.
    fn size(&self) -> (usize, usize) {
        (
            usize::try_from(self.rows).unwrap_or(0),
            usize::try_from(self.columns).unwrap_or(0),
        )
    }
}

/// The row the rank starts on, and how much lower each wave begins.
const START_Y: i32 = 2;
const WAVE_DROP: i32 = 1;
const MAX_WAVE_DROP: i32 = 4;

/// The gunner's rows: the muzzle, then the base.
const MUZZLE_ROW: i32 = canvas::HEIGHT - 2;
const BASE_ROW: i32 = canvas::HEIGHT - 1;
/// The base is three wide, so one pixel either side of its centre.
const BASE_HALF: i32 = 1;
/// Lives a run starts with. Losing one keeps the wave; losing the last starts
/// the run over.
const LIVES: u8 = 3;
/// Where the pips for those lives are drawn, which the rank never reaches.
const LIVES_ROW: i32 = 0;

/// The shelters, and how many hits each of their pixels takes.
const BUNKER_ROW: i32 = canvas::HEIGHT - 5;
const BUNKER_COLUMNS: [usize; 4] = [1, 2, 6, 7];
const BUNKER_HITS: u8 = 2;

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

/// Flashes a second while a loss is held.
const FLASH_RATE: f32 = 6.0;

/// Seconds the end of a wave, a lost life and a lost run are held.
const CLEARED_DELAY: f32 = 0.8;
const LOST_LIFE_DELAY: f32 = 0.9;
const LOST_RUN_DELAY: f32 = 1.4;
/// Longest simulated step, so a stalled thread does not teleport anything.
const MAX_STEP: f32 = 0.1;

/// Brightness of a bomb, a damaged bunker and a life still in hand.
const BOMB_LEVEL: u8 = 120;
const BUNKER_DAMAGED_LEVEL: u8 = 90;
const LIFE_LEVEL: u8 = 70;

/// What the game is doing.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Playing,
    /// The rank is down; the panel holds still before the next wave.
    Cleared(f32),
    /// A life is gone. The wave stands; the gunner comes back.
    LostLife(f32),
    /// The last life is gone, and the run starts over.
    LostRun(f32),
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
    alive: [[bool; MAX_COLUMNS]; MAX_ROWS],
    /// Which formation this wave is using.
    formation: usize,
    /// Top-left of the rank, in pixels.
    rank_x: i32,
    rank_y: i32,
    marching_right: bool,
    cannon: i32,
    lives: u8,
    /// Hits each column of shelter has left, zero where there is none.
    bunkers: [u8; canvas::WIDTH as usize],
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
    /// Starts a run.
    #[must_use]
    pub fn new(seed: Option<u64>, mode: ColorMode) -> Self {
        let mut game = Self {
            mode,
            rng: rng_from(seed),
            alive: [[false; MAX_COLUMNS]; MAX_ROWS],
            formation: 0,
            rank_x: 0,
            rank_y: START_Y,
            marching_right: true,
            cannon: canvas::WIDTH / 2,
            lives: LIVES,
            bunkers: [0; canvas::WIDTH as usize],
            bullet: None,
            bombs: Vec::new(),
            phase: Phase::Playing,
            wave: 0,
            step_timer: 0.0,
            bullet_timer: 0.0,
            bomb_timer: 0.0,
            cannon_timer: 0.0,
        };
        game.restart();
        game
    }

    /// The rank this wave is fighting.
    fn formation(&self) -> &'static Formation {
        &FORMATIONS[self.formation]
    }

    /// Sets a fresh rank up, a little lower each wave.
    fn start_wave(&mut self) {
        self.formation = usize::try_from(self.wave).unwrap_or(0) % FORMATIONS.len();
        let shape = self.formation();
        let (rows, columns) = shape.size();
        self.alive = [[false; MAX_COLUMNS]; MAX_ROWS];
        for row in 0..rows {
            for column in 0..columns {
                self.alive[row][column] = true;
            }
        }

        self.rank_x = (canvas::WIDTH - shape.width()) / 2;
        let drop = i32::try_from(self.wave).unwrap_or(0) * WAVE_DROP;
        self.rank_y = START_Y + drop.min(MAX_WAVE_DROP);
        self.marching_right = true;
        self.clear_the_air();
    }

    /// Everything in flight goes; the gunner comes back to the middle.
    fn clear_the_air(&mut self) {
        self.bullet = None;
        self.bombs.clear();
        self.cannon = canvas::WIDTH / 2;
        self.step_timer = 0.0;
    }

    /// Back to the first wave with a full complement of lives and shelter.
    fn restart(&mut self) {
        self.wave = 0;
        self.lives = LIVES;
        self.rebuild_bunkers();
        self.start_wave();
    }

    /// Fresh shelter, which a new run gets and a lost life does not.
    fn rebuild_bunkers(&mut self) {
        self.bunkers = [0; canvas::WIDTH as usize];
        for column in BUNKER_COLUMNS {
            self.bunkers[column] = BUNKER_HITS;
        }
    }

    /// Where an alien sits, whether or not it is alive.
    fn alien_at(&self, row: usize, column: usize) -> (i32, i32) {
        let spacing = self.formation().spacing;
        let step = |index: usize| i32::try_from(index).unwrap_or(0) * spacing;
        (self.rank_x + step(column), self.rank_y + step(row))
    }

    /// Every living alien, as a position.
    fn living(&self) -> impl Iterator<Item = (i32, i32)> + '_ {
        (0..MAX_ROWS).flat_map(move |row| {
            (0..MAX_COLUMNS)
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
        let shape = self.formation();
        let count = |value: usize| f32::from(u16::try_from(value).unwrap_or(u16::MAX));
        let (rows, columns) = shape.size();
        let total = count(rows * columns).max(2.0);
        let standing = count(self.remaining());
        let fraction = ((standing - 1.0) / (total - 1.0)).clamp(0.0, 1.0);
        STEP_FAST + (STEP_SLOW - STEP_FAST) * fraction
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
        for column in 0..MAX_COLUMNS {
            let found = (0..MAX_ROWS)
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

    /// Alien steps that fit into a bullet's climb from the muzzle to row `y`.
    fn steps_while_a_shot_climbs(&self, y: i32) -> i32 {
        let rows = f32::from(u8::try_from((MUZZLE_ROW - 1 - y).max(0)).unwrap_or(0));
        canvas::floor_pixel(rows * BULLET_STEP / self.step_interval())
    }

    /// Where an alien will be by the time a shot fired now could reach it.
    ///
    /// A bullet takes the better part of a second to climb the panel, and the
    /// rank walks a pixel every step while it climbs. Aiming at where an alien
    /// is means arriving two columns behind it, and a wave that never falls —
    /// which is exactly what happened before this existed. The march is walked
    /// rather than extrapolated, because a thin rank turns at a wall and comes
    /// back inside a single flight.
    fn predicted_x(&self, x: i32, y: i32) -> i32 {
        let steps = self.steps_while_a_shot_climbs(y);
        let Some((left, right)) = self.extent() else {
            return x;
        };

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

    /// Whether a column still has shelter over the gunner's head.
    fn sheltered(&self, column: i32) -> bool {
        usize::try_from(column)
            .is_ok_and(|column| self.bunkers.get(column).copied().unwrap_or(0) > 0)
    }

    /// How many rows away the soonest bomb aimed at `column` is.
    ///
    /// A bomb over an intact bunker is not a threat to that column, which is
    /// what makes the gunner duck behind the shelters instead of running the
    /// length of the panel.
    fn threat_to(&self, column: i32) -> Option<i32> {
        if self.sheltered(column) {
            return None;
        }
        self.bombs
            .iter()
            .filter(|bomb| (bomb.x - column).abs() <= BASE_HALF)
            .map(|bomb| BASE_ROW - bomb.y)
            .filter(|rows| *rows <= DODGE_ROWS)
            .min()
    }

    /// The column the gunner wants to be in.
    ///
    /// Safety first, then aim: it picks the safe column nearest the shot it
    /// wants to take. Stepping one pixel aside instead — the obvious thing —
    /// walked straight back under the bomb on the following tick.
    fn wanted_column(&self) -> i32 {
        let aim = self
            .living()
            .min_by_key(|(x, y)| (-y, (x - self.cannon).abs()))
            .map_or(self.cannon, |(x, y)| {
                self.predicted_x(x, y).clamp(0, canvas::WIDTH - 1)
            });
        let hunted = self
            .bombs
            .iter()
            .any(|bomb| BASE_ROW - bomb.y <= DODGE_ROWS);

        (0..canvas::WIDTH)
            .filter(|column| self.threat_to(*column).is_none())
            .min_by_key(|column| {
                // Its own shelter blocks its shots, so it only stands under one
                // when there is something to shelter from.
                let blocked = i32::from(self.sheltered(*column) && !hunted) * canvas::WIDTH;
                ((column - aim).abs() + blocked, (column - self.cannon).abs())
            })
            .unwrap_or_else(|| {
                // Nowhere is safe: stand where the bomb is furthest away.
                (0..canvas::WIDTH)
                    .max_by_key(|column| self.threat_to(*column).unwrap_or(i32::MAX))
                    .unwrap_or(self.cannon)
            })
    }

    /// Fires if the gunner is lined up, has nothing in the air, and would not
    /// simply demolish its own shelter.
    fn maybe_fire(&mut self) {
        if self.bullet.is_some() || self.sheltered(self.cannon) {
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

    /// Takes a hit out of the shelter in a column, if there is any.
    fn hit_bunker(&mut self, column: i32) -> bool {
        let Ok(column) = usize::try_from(column) else {
            return false;
        };
        match self.bunkers.get_mut(column) {
            Some(hits) if *hits > 0 => {
                *hits -= 1;
                true
            }
            _ => false,
        }
    }

    /// Moves the bullet up, taking an alien or a piece of shelter with it.
    fn step_bullet(&mut self) {
        let Some(mut bullet) = self.bullet else {
            return;
        };
        bullet.y -= 1;
        if bullet.y < 0 {
            self.bullet = None;
            return;
        }

        if bullet.y == BUNKER_ROW && self.hit_bunker(bullet.x) {
            self.bullet = None;
            return;
        }

        for row in 0..MAX_ROWS {
            for column in 0..MAX_COLUMNS {
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
        let mut absorbed: Vec<i32> = Vec::new();

        for bomb in &mut self.bombs {
            bomb.y += 1;
            if bomb.y == BUNKER_ROW {
                absorbed.push(bomb.x);
            } else if bomb.y >= MUZZLE_ROW && (bomb.x - self.cannon).abs() <= BASE_HALF {
                hit = true;
            }
        }

        let stopped: Vec<i32> = absorbed
            .into_iter()
            .filter(|column| self.hit_bunker(*column))
            .collect();
        self.bombs.retain(|bomb| {
            bomb.y < canvas::HEIGHT && !(bomb.y == BUNKER_ROW && stopped.contains(&bomb.x))
        });
        hit
    }

    /// Whether the rank has walked into the gunner.
    fn rank_has_landed(&self) -> bool {
        self.living().any(|(_, y)| y >= MUZZLE_ROW)
    }

    /// Takes a life, and says how long the panel should hold still for.
    fn lose_a_life(&mut self) -> Phase {
        self.lives = self.lives.saturating_sub(1);
        if self.lives == 0 {
            Phase::LostRun(LOST_RUN_DELAY)
        } else {
            Phase::LostLife(LOST_LIFE_DELAY)
        }
    }

    /// Draws the rank, the shelters, the gunner and everything in the air.
    fn draw_game(&self, canvas: &mut Canvas, area: Area) {
        for (x, y) in self.living() {
            canvas.set_max(x, area.row(y), u8::MAX);
        }

        let dim = |level: u8| {
            if self.mode == ColorMode::Bw {
                u8::MAX
            } else {
                level
            }
        };
        for (column, hits) in self.bunkers.iter().enumerate() {
            if *hits == 0 {
                continue;
            }
            let level = if *hits < BUNKER_HITS {
                dim(BUNKER_DAMAGED_LEVEL)
            } else {
                u8::MAX
            };
            let column = i32::try_from(column).unwrap_or(0);
            canvas.set_max(column, area.row(BUNKER_ROW), level);
        }

        for bomb in &self.bombs {
            canvas.set_max(bomb.x, area.row(bomb.y), dim(BOMB_LEVEL));
        }
        if let Some(bullet) = self.bullet {
            canvas.set_max(bullet.x, area.row(bullet.y), u8::MAX);
        }

        // A pip per life still in hand, on a row the rank never reaches.
        for life in 0..i32::from(self.lives.saturating_sub(1)) {
            canvas.set_max(life, area.row(LIVES_ROW), dim(LIFE_LEVEL));
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
            Phase::LostLife(remaining) => {
                let remaining = remaining - dt;
                self.phase = if remaining <= 0.0 {
                    // The wave stands: only the gunner starts again.
                    self.clear_the_air();
                    Phase::Playing
                } else {
                    Phase::LostLife(remaining)
                };
                return;
            }
            Phase::LostRun(remaining) => {
                let remaining = remaining - dt;
                self.phase = if remaining <= 0.0 {
                    self.restart();
                    Phase::Playing
                } else {
                    Phase::LostRun(remaining)
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
                self.phase = self.lose_a_life();
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
            // Being overrun costs the run, not a life: the rank is still there.
            self.lives = 1;
            self.phase = self.lose_a_life();
        }
    }

    fn render(&self, canvas: &mut Canvas, area: Area) {
        match self.phase {
            Phase::LostLife(remaining) | Phase::LostRun(remaining) => {
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
    use super::{
        BASE_HALF, BUNKER_COLUMNS, BUNKER_ROW, FORMATIONS, Invaders, LIVES, MAX_COLUMNS, MAX_ROWS,
        MUZZLE_ROW, Phase, Shot,
    };
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
        let steps = canvas::floor_pixel(seconds / frame.as_secs_f32());
        for _ in 0..steps {
            game.update(frame);
        }
    }

    #[test]
    fn every_formation_has_room_to_march() {
        // A rank as wide as the panel bounces off a wall every other step and
        // descends with it, so it lands on the gunner whatever anyone does.
        for (index, shape) in FORMATIONS.iter().enumerate() {
            let (rows, columns) = shape.size();
            assert!(columns <= MAX_COLUMNS, "formation {index} is too wide");
            assert!(rows <= MAX_ROWS, "formation {index} is too tall");
            let travel = canvas::WIDTH - shape.width();
            assert!(
                travel >= 3,
                "formation {index} has {travel} columns of travel and will rain down"
            );
        }
    }

    #[test]
    fn waves_do_not_all_look_the_same() {
        // The complaint that started this: one rank, over and over.
        let mut game = game();
        let mut seen = std::collections::HashSet::new();
        for wave in 0..u32::try_from(FORMATIONS.len()).expect("a few formations") {
            game.wave = wave;
            game.start_wave();
            seen.insert((
                game.formation().columns,
                game.formation().rows,
                game.formation().spacing,
            ));
        }
        assert!(seen.len() > 1, "every wave fields the same rank");
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
        let area = Area {
            top: 0,
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

    #[test]
    fn a_lost_life_keeps_the_wave_standing() {
        // Restarting the wave on every hit is what made a run feel like the
        // same twenty seconds forever.
        let mut game = game();
        play(&mut game, 6.0);
        let standing = game.remaining();
        let (rows, columns) = FORMATIONS[0].size();
        assert!(standing < rows * columns, "nothing was shot in six seconds");

        game.bombs = vec![Shot {
            x: game.cannon,
            y: MUZZLE_ROW - 1,
        }];
        assert!(game.step_bombs());
        game.phase = game.lose_a_life();
        assert!(matches!(game.phase, Phase::LostLife(_)));
        play(&mut game, 1.5);

        assert_eq!(game.remaining(), standing, "the rank was set back up");
        assert_eq!(game.lives, LIVES - 1, "the life was not counted");
    }

    #[test]
    fn a_run_ends_only_when_the_lives_do() {
        let mut game = game();
        for _ in 1..LIVES {
            game.phase = game.lose_a_life();
            assert!(matches!(game.phase, Phase::LostLife(_)));
        }
        game.phase = game.lose_a_life();
        assert!(matches!(game.phase, Phase::LostRun(_)), "it kept playing");

        play(&mut game, 2.0);
        assert_eq!(game.lives, LIVES, "the run did not start over");
        assert_eq!(game.wave, 0);
    }

    #[test]
    fn a_bunker_stops_a_bomb_and_wears_out() {
        let mut game = game();
        let column = i32::try_from(BUNKER_COLUMNS[0]).expect("a column");
        game.cannon = column;
        for hit in 0..super::BUNKER_HITS {
            game.bombs = vec![Shot {
                x: column,
                y: BUNKER_ROW - 1,
            }];
            assert!(!game.step_bombs(), "hit {hit} reached the gunner");
            assert!(game.bombs.is_empty(), "hit {hit} carried on falling");
        }
        assert_eq!(
            game.bunkers[BUNKER_COLUMNS[0]], 0,
            "the shelter held forever"
        );

        // Worn through, the next one gets past it.
        game.bombs = vec![Shot {
            x: column,
            y: BUNKER_ROW - 1,
        }];
        game.step_bombs();
        assert!(
            !game.bombs.is_empty(),
            "a spent bunker still stopped a bomb"
        );
    }

    #[test]
    fn the_gunner_does_not_shoot_its_own_shelter_away() {
        let mut game = game();
        game.cannon = i32::try_from(BUNKER_COLUMNS[0]).expect("a column");
        game.bullet = None;
        game.maybe_fire();
        assert!(game.bullet.is_none(), "it fired into its own bunker");
    }

    #[test]
    fn the_gunner_shelters_when_something_is_falling() {
        let mut game = game();
        game.cannon = 4;
        game.bombs = vec![Shot {
            x: 4,
            y: super::BASE_ROW - 6,
        }];
        let wanted = game.wanted_column();
        assert!(
            (wanted - 4).abs() > BASE_HALF,
            "it stayed at {wanted}, under the bomb at 4"
        );
    }

    #[test]
    fn a_bomb_reaching_the_gunner_is_a_hit_and_one_beside_it_is_not() {
        // The collision rule on its own: driving this through `update` tests
        // the dodge instead, and the dodge is good enough to get out of the
        // way, which is a different thing worth a different test.
        let mut game = game();
        game.cannon = 4;
        game.bunkers = [0; canvas::WIDTH as usize];
        game.bombs = vec![Shot {
            x: 4,
            y: MUZZLE_ROW - 1,
        }];
        assert!(game.step_bombs(), "the gunner shrugged off a direct hit");

        let mut beside = self::game();
        beside.cannon = 4;
        beside.bunkers = [0; canvas::WIDTH as usize];
        beside.bombs = vec![Shot {
            x: 4 + BASE_HALF + 1,
            y: MUZZLE_ROW - 1,
        }];
        assert!(!beside.step_bombs(), "a near miss counted as a hit");
    }

    #[test]
    fn the_gunner_clears_waves_rather_than_stalling() {
        // A gunner that never finishes would show the same rank until the
        // laptop is closed. It is allowed to lose lives on the way.
        let mut game = game();
        let mut waves = 0;
        for _ in 0..3600 {
            game.update(Duration::from_millis(33));
            waves = waves.max(game.wave);
        }
        assert!(waves >= 2, "only {waves} waves fell in two minutes");
    }

    #[test]
    fn a_shot_lands_where_it_was_led() {
        // Checked against the march itself rather than against a formula: the
        // lead has to survive the rank turning at a wall, which is most of what
        // it does once only a few aliens are left.
        for survivors in [12, 3, 1] {
            let mut game = game();
            let mut left = MAX_ROWS * MAX_COLUMNS;
            for row in 0..MAX_ROWS {
                for column in 0..MAX_COLUMNS {
                    if game.alive[row][column] && left > survivors {
                        game.alive[row][column] = false;
                        left -= 1;
                    }
                }
            }

            let Some((x, y)) = game.living().next() else {
                continue;
            };
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
    fn the_gunner_is_always_drawn() {
        // Whatever else is happening, the panel has to show who is playing.
        let mut game = game();
        for _ in 0..40 {
            play(&mut game, 0.3);
            if matches!(game.phase, Phase::LostLife(_) | Phase::LostRun(_)) {
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
