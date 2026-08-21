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
use crate::font;
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
/// Where a shot appears, leaving a dark row between the gunner and its own
/// bullet. The muzzle is a lone lit pixel and so is a bullet: launched one row
/// up they touched, and the pair read as a single two-pixel object that then
/// tore itself in half. A gap is the only thing that separates them in black
/// and white.
const LAUNCH_ROW: i32 = MUZZLE_ROW - 2;
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
const BULLET_STEP: f32 = 0.025;
const BOMB_STEP: f32 = 0.13;
/// Seconds the gunner takes to slide one column.
const CANNON_STEP: f32 = 0.07;

/// Chance that the rank drops a bomb on any one of its steps, and how much
/// each wave adds to it.
const BOMB_CHANCE: f32 = 0.22;
const BOMB_CHANCE_PER_WAVE: f32 = 0.03;
const MAX_BOMB_CHANCE: f32 = 0.55;

/// Bombs in the air at once, and the wave from which a further one is allowed.
///
/// Four at once on a panel nine wide leaves the gunner nowhere to stand, so
/// that is where a run ends rather than a difficulty it is expected to survive.
const MAX_BOMBS: usize = 2;
const BOMBS_CEILING: usize = 4;
const WAVES_PER_BOMB: u32 = 4;

/// How much of the step interval each wave takes off, and the floor.
///
/// A run has to get harder or it has no shape: with the arithmetic bugs fixed
/// the gunner cleared waves indefinitely, and every wave past the fourth was
/// pixel for pixel a wave it had already played.
const SPEED_PER_WAVE: f32 = 0.04;
const MIN_SPEED_SCALE: f32 = 0.55;

/// How far above the gunner a bomb starts counting as a reason to move.
///
/// Sidestepping takes a seventh of a second; looking two seconds ahead made
/// the gunner set off for trouble that had not happened yet, and put itself in
/// danger more often than the sky did.
const DODGE_ROWS: i32 = 8;
/// How many columns of aim the gunner will give up to keep its shelter.
const SHELTER_RELUCTANCE: i32 = 2;

// A rank must be able to march, a wave must start above the row a shot can
// reach, and every step must take time — a zero interval would spin `update`
// for ever and freeze the panel in silence.
const _: () = assert!(STEP_FAST > 0.0 && STEP_SLOW >= STEP_FAST);
const _: () = assert!(LAUNCH_ROW > BUNKER_ROW && BUNKER_ROW > 1);
// The wave label has to finish above the shelters it is written over.
const _: () = assert!(WAVE_LABEL_ROW + font::GLYPH_HEIGHT < BUNKER_ROW);

/// Flashes a second while a loss is held.
const FLASH_RATE: f32 = 6.0;

/// Where the wave just cleared is written, and how far the burst of a lost
/// life reaches from the gunner. The label is five rows tall and has to clear
/// the shelters below it.
const WAVE_LABEL_ROW: i32 = 21;
const BURST_RADIUS: i32 = 4;

/// Seconds the end of a wave, a lost life and a lost run are held.
const CLEARED_DELAY: f32 = 0.8;
const LOST_LIFE_DELAY: f32 = 0.9;
const LOST_RUN_DELAY: f32 = 1.4;
/// Longest simulated step, so a stalled thread does not teleport anything.
const MAX_STEP: f32 = 0.1;

/// Brightness of a bomb and of a life still in hand, for the greyscale mode
/// this scene does not use by default. Damage to a shelter is drawn as a shape
/// rather than a shade, so it survives the black and white it actually runs in.
const BOMB_LEVEL: u8 = 120;
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
        // Shelter for every wave. Rebuilt once a run, it was absent from the
        // panel about half the time, taking the gunner's best trick with it.
        self.rebuild_bunkers();
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
        let thinning = STEP_FAST + (STEP_SLOW - STEP_FAST) * fraction;
        thinning * self.speed_scale()
    }

    /// How much faster this wave marches than the first.
    fn speed_scale(&self) -> f32 {
        let waves = f32::from(u16::try_from(self.wave).unwrap_or(u16::MAX));
        (1.0 - waves * SPEED_PER_WAVE).max(MIN_SPEED_SCALE)
    }

    /// Bombs this wave may have in the air at once.
    fn bombs_allowed(&self) -> usize {
        let extra = usize::try_from(self.wave / WAVES_PER_BOMB).unwrap_or(0);
        MAX_BOMBS.saturating_add(extra).min(BOMBS_CEILING)
    }

    /// How likely the rank is to drop one on a given step.
    fn bomb_chance(&self) -> f32 {
        let waves = f32::from(u16::try_from(self.wave).unwrap_or(u16::MAX));
        (BOMB_CHANCE + waves * BOMB_CHANCE_PER_WAVE).min(MAX_BOMB_CHANCE)
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
        if self.bombs.len() >= self.bombs_allowed()
            || self.rng.random::<f32>() >= self.bomb_chance()
        {
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
        let rows = f32::from(u8::try_from((LAUNCH_ROW - y).max(0)).unwrap_or(0));
        // Counting from zero assumed the rank steps the instant the shot goes
        // off. It steps when its own timer comes round, so more than half of
        // all flights took one step more than predicted — and a shot led one
        // column short never hits anything.
        let elapsed = rows * BULLET_STEP + self.step_timer;
        canvas::floor_pixel(elapsed / self.step_interval())
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
        self.bombs
            .iter()
            .filter(|bomb| (bomb.x - column).abs() <= BASE_HALF && !self.sheltered(bomb.x))
            .map(|bomb| MUZZLE_ROW - bomb.y)
            .filter(|rows| *rows <= DODGE_ROWS)
            .min()
    }

    /// The column the gunner wants to be in.
    ///
    /// Safety first, then aim: it picks the safe column nearest the shot it
    /// wants to take. Stepping one pixel aside instead — the obvious thing —
    /// walked straight back under the bomb on the following tick.
    fn wanted_column(&self) -> i32 {
        // Whatever is easiest to hit, not whatever is lowest. Chasing the
        // lowest alien dragged the gunner from one wall to the other every
        // time the rank turned — a metronome that spent its life walking
        // instead of shooting, and got hit crossing the panel.
        let aim = self
            .living()
            .map(|(x, y)| self.predicted_x(x, y).clamp(0, canvas::WIDTH - 1))
            .min_by_key(|x| (x - self.cannon).abs())
            .unwrap_or(self.cannon);

        (0..canvas::WIDTH)
            .filter(|column| self.threat_to(*column).is_none() && self.reachable(*column))
            .min_by_key(|column| {
                // Its own shelter blocks its shots, so it is a little reluctant
                // to stand under one — but only a little, or it bounces out
                // from cover the moment the sky is clear.
                let blocked = i32::from(self.sheltered(*column)) * SHELTER_RELUCTANCE;
                ((column - aim).abs() + blocked, (column - self.cannon).abs())
            })
            .unwrap_or_else(|| {
                // Nowhere is safe: stand where the bomb is furthest away.
                (0..canvas::WIDTH)
                    .max_by_key(|column| self.threat_to(*column).unwrap_or(i32::MAX))
                    .unwrap_or(self.cannon)
            })
    }

    /// Whether the gunner can walk to `column` without passing under a bomb
    /// that lands before it gets there.
    ///
    /// Only the destination used to be checked, so the gunner would set off
    /// across the panel towards shelter and walk into the very bomb it was
    /// running from.
    fn reachable(&self, column: i32) -> bool {
        let seconds =
            |steps: i32, each: f32| f32::from(u8::try_from(steps.max(0)).unwrap_or(u8::MAX)) * each;
        let walk = seconds((column - self.cannon).abs(), CANNON_STEP);
        let (from, to) = (
            self.cannon.min(column) - BASE_HALF,
            self.cannon.max(column) + BASE_HALF,
        );

        !self.bombs.iter().any(|bomb| {
            let crosses = (from..=to).contains(&bomb.x) && !self.sheltered(bomb.x);
            // A bomb kills on reaching the muzzle, a row above the base.
            // Measuring to the base credited every one of them with an extra
            // row — nearly two cannon steps — and that arithmetic was doing
            // three quarters of the killing in this game.
            crosses && seconds(MUZZLE_ROW - bomb.y, BOMB_STEP) <= walk
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
                y: LAUNCH_ROW,
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
        // Spent at the guard row: going all the way to the top put a bullet in
        // the lives row, where it read as an extra life, or on the dark row
        // that keeps the pips countable. Aliens never climb above `START_Y`,
        // so nothing becomes unshootable.
        if bullet.y <= LIVES_ROW + 1 {
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
        let mut spent: Vec<usize> = Vec::new();

        for index in 0..self.bombs.len() {
            let bomb = &mut self.bombs[index];
            bomb.y += 1;
            // At or below the shelter row, not exactly on it: a bomb born on
            // that row from an alien standing over it stepped straight past
            // and the equality never held, while the hit on the gunner one
            // line down has always used `>=`.
            let (x, y) = (bomb.x, bomb.y);
            if y >= BUNKER_ROW && self.hit_bunker(x) {
                // One hit per bomb. Collecting the columns and charging them
                // afterwards let two bombs sharing a cell cost two hits and be
                // stopped once between them.
                spent.push(index);
            } else if y >= MUZZLE_ROW && (x - self.cannon).abs() <= BASE_HALF {
                hit = true;
            }
        }

        let mut index = 0;
        self.bombs.retain(|bomb| {
            let stopped = spent.contains(&index);
            index += 1;
            // Gone by the base row: one left sitting there made the gunner
            // read as four wide, or as a bar with a lump beside it.
            !stopped && bomb.y < BASE_ROW
        });
        hit
    }

    /// Whether the rank has walked in past the point a shot can reach.
    ///
    /// Measured from the launch row, not the muzzle: `step_bullet` moves before
    /// it tests, so an alien sitting on the launch row or below it can be lined
    /// up, fired at, and never hit — a legal state that wastes the barrel for
    /// the whole flight.
    fn rank_has_landed(&self) -> bool {
        self.living().any(|(_, y)| y >= LAUNCH_ROW)
    }

    /// Lets everything in flight carry on while the game is between waves.
    fn settle(&mut self, dt: f32) {
        self.cannon_timer += dt;
        while self.cannon_timer >= CANNON_STEP {
            self.cannon_timer -= CANNON_STEP;
            let home = canvas::WIDTH / 2;
            self.cannon += (home - self.cannon).signum();
        }

        self.bullet_timer += dt;
        while self.bullet_timer >= BULLET_STEP {
            self.bullet_timer -= BULLET_STEP;
            self.step_bullet();
        }

        self.bomb_timer += dt;
        while self.bomb_timer >= BOMB_STEP {
            self.bomb_timer -= BOMB_STEP;
            self.step_bombs();
        }
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

    /// Writes the wave just finished, during the beat between waves.
    ///
    /// The only moment the panel has to spare, and the only thing that says
    /// where a run has got to: without it every wave past the fourth looked
    /// like one already played, and a run had no shape a watcher could see.
    fn draw_wave(&self, canvas: &mut Canvas, area: Area) {
        // The number of the wave cleared, counting from one.
        let shown = (self.wave + 1) % 100;
        let (tens, units) = (shown / 10, shown % 10);
        let width = if tens > 0 {
            font::GLYPH_WIDTH * 2 + 1
        } else {
            font::GLYPH_WIDTH
        };
        let left = (canvas::WIDTH - width) / 2;

        if tens > 0 {
            font::draw_digit(canvas, tens, left, area.row(WAVE_LABEL_ROW), u8::MAX);
            font::draw_digit(
                canvas,
                units,
                left + font::GLYPH_WIDTH + 1,
                area.row(WAVE_LABEL_ROW),
                u8::MAX,
            );
        } else {
            font::draw_digit(canvas, units, left, area.row(WAVE_LABEL_ROW), u8::MAX);
        }
    }

    /// The gunner coming apart where it stood.
    ///
    /// A lost life is a common event — one every couple of minutes — and it
    /// used to fire the same full-panel white strobe as the end of a run,
    /// under the owner's hands. A burst says the same thing where it happened.
    fn draw_burst(&self, canvas: &mut Canvas, area: Area, age: f32) {
        self.draw_game(canvas, area);

        let reach = canvas::floor_pixel(age * f32::from(u8::try_from(BURST_RADIUS).unwrap_or(1)));
        for step in 0..=reach.min(BURST_RADIUS) {
            for x in [self.cannon - step, self.cannon + step] {
                canvas.set_max(x, area.row(BASE_ROW), u8::MAX);
                canvas.set_max(x, area.row(MUZZLE_ROW - step), u8::MAX);
            }
        }
    }

    /// Draws the rank, the shelters, the gunner and everything in the air.
    fn draw_game(&self, canvas: &mut Canvas, area: Area) {
        for (x, y) in self.living() {
            canvas.set_max(x, area.row(y), u8::MAX);
        }

        // Shading is not available here: this scene runs in black and white so
        // that it can have thirty frames a second, and the greyscale levels
        // below only ever apply if someone forces `--color-mode greyscale`.
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
            // Two rows, crumbling from the top. In black and white a single
            // row has two pictures for three states, so a fresh shelter and
            // one hit from collapse looked identical — and a half-dead pair
            // read as two loose bombs rather than as shelter at all.
            let column = i32::try_from(column).unwrap_or(0);
            canvas.set_max(column, area.row(BUNKER_ROW), u8::MAX);
            if *hits == BUNKER_HITS {
                canvas.set_max(column, area.row(BUNKER_ROW - 1), u8::MAX);
            }
        }

        for bomb in &self.bombs {
            canvas.set_max(bomb.x, area.row(bomb.y), dim(BOMB_LEVEL));
        }
        if let Some(bullet) = self.bullet {
            canvas.set_max(bullet.x, area.row(bullet.y), u8::MAX);
        }

        // A pip per life still in hand, spaced so they can be counted: two
        // adjacent pixels read as one bar through a diffuser. It used to draw
        // one fewer than it had, so the last life showed an empty row.
        for life in 0..i32::from(self.lives) {
            canvas.set_max(life * 2, area.row(LIVES_ROW), dim(LIFE_LEVEL));
        }

        // The base leans rather than clipping: `hline` used to lose a pixel at
        // either wall, and a two-pixel corner does not read as a cannon. Only
        // the footing moves — the muzzle, which is what aims, stays put.
        let footing = self.cannon.clamp(BASE_HALF, canvas::WIDTH - 1 - BASE_HALF);
        canvas.hline(
            footing - BASE_HALF,
            footing + BASE_HALF,
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
                if remaining <= 0.0 {
                    self.wave += 1;
                    self.start_wave();
                    self.phase = Phase::Playing;
                    return;
                }
                // The sky finishes emptying and the gunner walks home, rather
                // than the panel holding a still image for eight tenths of a
                // second — which happened sixty-six times in eleven minutes
                // and is indistinguishable from a scene that has crashed.
                self.phase = Phase::Cleared(remaining);
                self.settle(dt);
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

        // The bullet moves before the gun fires, so a shot is drawn on the row
        // it launched from. Firing first meant every bullet stepped off that
        // row before the frame was painted — the launch was invisible and the
        // shot appeared level with the shelters, fused to them one time in
        // seven. The gap under the muzzle only works if the launch is seen.
        self.bullet_timer += dt;
        while self.bullet_timer >= BULLET_STEP {
            self.bullet_timer -= BULLET_STEP;
            self.step_bullet();
        }

        self.cannon_timer += dt;
        while self.cannon_timer >= CANNON_STEP {
            self.cannon_timer -= CANNON_STEP;
            let wanted = self.wanted_column();
            self.cannon += (wanted - self.cannon).signum();
            self.cannon = self.cannon.clamp(0, canvas::WIDTH - 1);
            self.maybe_fire();
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
            Phase::LostLife(remaining) => {
                self.draw_burst(canvas, area, LOST_LIFE_DELAY - remaining);
            }
            Phase::LostRun(remaining) => {
                // The whole panel, for the one event that ends a run. Rare
                // enough now to be worth the light.
                if canvas::floor_pixel(remaining * FLASH_RATE) % 2 == 0 {
                    for y in 0..area.height {
                        canvas.hline(0, canvas::WIDTH - 1, area.row(y), u8::MAX);
                    }
                } else {
                    self.draw_game(canvas, area);
                }
            }
            Phase::Cleared(_) => {
                self.draw_game(canvas, area);
                self.draw_wave(canvas, area);
            }
            Phase::Playing => self.draw_game(canvas, area),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BASE_HALF, BUNKER_COLUMNS, BUNKER_ROW, FORMATIONS, Invaders, LIVES, LIVES_ROW, MAX_COLUMNS,
        MAX_ROWS, MUZZLE_ROW, Phase, Shot,
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
    fn the_gunner_shoots_what_is_nearest_rather_than_what_is_lowest() {
        // Chasing the lowest alien walked it wall to wall every time the rank
        // turned: a metronome that spent its life travelling instead of
        // shooting, and got hit on the way.
        let mut game = game();
        game.bombs.clear();
        game.cannon = 1;
        game.alive = [[false; MAX_COLUMNS]; MAX_ROWS];
        // Both low, so the lead is a pixel and the choice is about distance:
        // one under the gunner, one across the panel.
        game.rank_x = 0;
        game.rank_y = 26;
        game.alive[0][0] = true;
        game.alive[0][2] = true;

        let wanted = game.wanted_column();
        let far = game.alien_at(0, 2).0;
        assert!(
            (wanted - game.cannon).abs() < (far - game.cannon).abs(),
            "it set off for the far one at {far} instead of staying near {}",
            game.cannon
        );
    }

    #[test]
    fn the_gunner_will_not_walk_under_a_falling_bomb() {
        // Only the destination used to be checked, so it would set off across
        // the panel for shelter and walk into the bomb it was running from.
        let mut game = game();
        game.bunkers = [0; canvas::WIDTH as usize];
        game.cannon = 0;
        game.bombs = vec![Shot {
            x: 4,
            y: super::BASE_ROW - 2,
        }];

        assert!(
            !game.reachable(8),
            "it walked the length of the panel under a bomb"
        );
        assert!(game.reachable(1), "it refused a step it had time for");
    }

    #[test]
    fn a_bomb_beside_a_sheltered_gunner_is_still_a_threat() {
        // A shelter is one pixel wide and only stops what falls in its own
        // column; the base is three wide. Asking whether the gunner's column
        // was sheltered made a bomb one column over read as harmless — and a
        // bunker column advertise itself as the nearest safe place to stand.
        let mut game = game();
        // The outer column of a pair, so the bomb falls on bare floor beside
        // it rather than onto the other half of the same shelter.
        let shelter = i32::try_from(BUNKER_COLUMNS[1]).expect("a column");
        game.cannon = shelter;
        let beside = shelter + 1;
        assert!(
            !game.sheltered(beside),
            "the test picked a sheltered column"
        );
        game.bombs = vec![Shot {
            x: beside,
            y: MUZZLE_ROW - 4,
        }];
        assert!(
            game.threat_to(shelter).is_some(),
            "a bomb that will land on the base was called safe"
        );
    }

    #[test]
    fn a_walk_is_measured_to_the_row_a_bomb_kills_on() {
        // A bomb kills at the muzzle, a row above the base. Measuring to the
        // base gave every one of them an extra row — nearly two cannon steps
        // — and that arithmetic was doing most of the killing.
        let mut game = game();
        game.bunkers = [0; canvas::WIDTH as usize];
        game.cannon = 0;
        // Placed so the bomb lands during a walk of four columns, but not
        // during a walk measured one row too generously.
        let walk = 4;
        let rows = canvas::floor_pixel(
            f32::from(u8::try_from(walk).unwrap_or(0)) * super::CANNON_STEP / super::BOMB_STEP,
        );
        game.bombs = vec![Shot {
            x: 2,
            y: MUZZLE_ROW - rows,
        }];
        assert!(
            !game.reachable(walk),
            "it set off across a bomb that lands on the way"
        );
    }

    #[test]
    fn the_pips_count_the_lives() {
        // They used to draw one fewer than there were, so the last life showed
        // an empty row, and they sat side by side where two lit pixels read as
        // one bar rather than two pips.
        let mut game = game();
        for lives in 1..=LIVES {
            game.lives = lives;
            let mut canvas = Canvas::new();
            game.render(&mut canvas, Area::FULL);
            let pips = (0..canvas::WIDTH)
                .filter(|x| canvas.get(*x, LIVES_ROW) > 0)
                .count();
            assert_eq!(pips, usize::from(lives), "{lives} lives drew {pips} pips");
        }
    }

    #[test]
    fn a_shot_is_drawn_on_the_row_it_launched_from() {
        // A bullet crosses a row faster than a frame lasts, so firing before
        // moving it meant the launch row was never painted: shots appeared
        // level with the shelters, fused to them one time in seven.
        let mut game = game();
        let mut seen = false;
        for _ in 0..2000 {
            game.update(Duration::from_millis(33));
            let Some(bullet) = game.bullet else { continue };
            if bullet.y != super::LAUNCH_ROW {
                continue;
            }
            let mut canvas = Canvas::new();
            game.render(&mut canvas, Area::FULL);
            assert!(
                canvas.get(bullet.x, super::LAUNCH_ROW) > 0,
                "a shot on the launch row was not drawn there"
            );
            seen = true;
        }
        assert!(seen, "no shot was ever caught on its launch row");
    }

    #[test]
    fn a_run_gets_harder_than_the_wave_it_started_on() {
        // Every wave past the fourth used to be pixel for pixel a wave already
        // played: the only thing that changed with the wave number was capped.
        let mut game = game();
        let (chance, bombs, speed) = (game.bomb_chance(), game.bombs_allowed(), game.speed_scale());

        game.wave = 12;
        assert!(game.bomb_chance() > chance, "the rank bombs no harder");
        assert!(game.bombs_allowed() > bombs, "the sky never fills");
        assert!(game.speed_scale() < speed, "the rank never speeds up");

        // And it stops somewhere: a run must end, not become impossible.
        game.wave = 5000;
        assert!(game.bombs_allowed() <= super::BOMBS_CEILING);
        assert!(game.speed_scale() >= super::MIN_SPEED_SCALE);
        assert!(game.step_interval() > 0.0, "a zero interval would spin");
    }

    #[test]
    fn the_wave_is_written_out_between_waves() {
        // The beat between waves is the only moment the panel has to spare,
        // and the wave number is the only thing that says where a run has got
        // to — without it every wave past the fourth looked like one already
        // played.
        let mut game = game();
        for wave in [0, 6, 11, 98] {
            game.wave = wave;
            game.phase = Phase::Cleared(0.5);
            let mut canvas = Canvas::new();
            game.render(&mut canvas, Area::FULL);

            let label: usize = (super::WAVE_LABEL_ROW
                ..super::WAVE_LABEL_ROW + crate::font::GLYPH_HEIGHT)
                .flat_map(|y| (0..canvas::WIDTH).map(move |x| (x, y)))
                .filter(|(x, y)| canvas.get(*x, *y) > 0)
                .count();
            assert!(label > 0, "wave {wave} was not written out");

            // And it keeps clear of the shelters underneath it.
            for x in 0..canvas::WIDTH {
                let sheltered = game.sheltered(x);
                assert_eq!(
                    canvas.get(x, BUNKER_ROW) > 0,
                    sheltered,
                    "the label spilled onto the shelter row at {x}"
                );
            }
        }
    }

    #[test]
    fn a_lost_life_is_a_local_event_and_a_lost_run_is_not() {
        // A life goes every couple of minutes, and it used to fire the same
        // full-panel strobe as the end of a run — the brightest thing the
        // panel does, under someone's hands, carrying one unreadable bit.
        let lit = |phase| {
            let mut game = game();
            game.cannon = 4;
            game.phase = phase;
            let mut canvas = Canvas::new();
            game.render(&mut canvas, Area::FULL);
            (0..canvas::HEIGHT)
                .flat_map(|y| (0..canvas::WIDTH).map(move |x| (x, y)))
                .filter(|(x, y)| canvas.get(*x, *y) > 0)
                .count()
        };

        let life = lit(Phase::LostLife(super::LOST_LIFE_DELAY - 0.6));
        let whole = usize::try_from(canvas::WIDTH * canvas::HEIGHT).expect("a panel");
        assert!(
            life < whole / 4,
            "losing a life lit {life} of {whole} pixels"
        );
        assert_eq!(
            lit(Phase::LostRun(super::LOST_RUN_DELAY)),
            whole,
            "losing a run should still take the whole panel"
        );
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
    fn the_scene_demands_the_whole_panel_and_keeps_its_guard_row() {
        // What was here asked about the rows outside a full-panel area, of
        // which there are none: the loop body never ran and it passed on
        // nothing for its whole life. This scene writes every row it draws as
        // an absolute constant, so the contract worth asserting is that it can
        // never be handed less than the panel — and that the one row it
        // promises to leave dark stays dark.
        assert_eq!(
            game().min_height(),
            canvas::HEIGHT,
            "a scene drawing in absolute rows must ask for all of them"
        );

        let mut game = game();
        for _ in 0..400 {
            play(&mut game, 0.3);
            let mut canvas = Canvas::new();
            game.render(&mut canvas, Area::FULL);
            if matches!(game.phase, Phase::LostLife(_) | Phase::LostRun(_)) {
                continue;
            }
            for x in 0..canvas::WIDTH {
                assert_eq!(
                    canvas.get(x, LIVES_ROW + 1),
                    0,
                    "the guard row under the pips was drawn on at {x}"
                );
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

        assert!(
            game.remaining() <= standing,
            "the rank was set back up: {} standing, was {standing}",
            game.remaining()
        );
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
            // The living count, not the size of the array: seeding this with
            // the array meant the twelve-alien case really tested nine, and
            // the full rank — longest flight, longest lead — went unexercised.
            let mut left = game.remaining();
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
    fn the_gunner_and_its_own_shot_never_touch() {
        // Both are lone lit pixels and there is no shading to tell them apart,
        // so a bullet launched right above the muzzle read as one two-pixel
        // object that then tore itself in half.
        let mut game = game();
        game.bunkers = [0; canvas::WIDTH as usize];
        game.bullet = None;
        game.alive = [[false; MAX_COLUMNS]; MAX_ROWS];
        game.rank_x = 4;
        game.rank_y = 20;
        game.alive[0][0] = true;
        // Stand where the shot is led to, so it takes the shot.
        let (x, y) = game.living().next().expect("one alien");
        game.cannon = game.predicted_x(x, y);
        game.maybe_fire();

        let bullet = game.bullet.expect("it did not fire at a lined-up alien");
        assert!(
            MUZZLE_ROW - bullet.y >= 2,
            "the shot appeared at row {}, touching the muzzle at {MUZZLE_ROW}",
            bullet.y
        );

        // And the panel agrees: a dark row between them.
        let mut canvas = Canvas::new();
        game.render(&mut canvas, Area::FULL);
        assert_eq!(
            canvas.get(game.cannon, MUZZLE_ROW - 1),
            0,
            "the gap between the gunner and its shot was drawn over"
        );
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
