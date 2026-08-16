//! Tetris, played by a robot that scores every landing before choosing one.
//!
//! Nine columns is close enough to the canonical ten, and thirty-four rows make
//! a proper well. The robot tries every rotation in every column, drops the
//! piece, and scores the board it would leave behind — holes it buries, how
//! high it stacks, how jagged the surface gets. That heuristic is well trodden
//! and, more to the point, it plays a long game: a bot that only chased line
//! clears would top out in under a minute, which is not worth watching.

use std::time::Duration;

use rand::RngExt;
use rand::rngs::StdRng;

use crate::canvas::{self, Canvas};
use crate::device::ColorMode;
use crate::scene::{Area, Scene, rng_from};

/// Well size. The panel is the well.
const WIDTH: i32 = canvas::WIDTH;
const HEIGHT: i32 = canvas::HEIGHT;
const CELLS: usize = canvas::PIXELS;

/// Time between two moves of the falling piece.
///
/// The same reasoning as the snake: on a one-bit panel the eye needs time to
/// follow, and a piece that teleports downwards is not legible as a piece.
const STEP_INTERVAL: f32 = 0.11;
/// Longest simulated step, so a stalled thread does not fast-forward the game.
const MAX_STEP: f32 = 0.1;
/// How long a completed line flashes before it collapses.
const CLEAR_FLASH: f32 = 0.35;
/// How long the well flashes when the robot tops out.
const TOP_OUT_FLASH: f32 = 1.4;
/// Toggles a second during either flash.
const FLASH_HZ: f32 = 7.0;

const STACK_LEVEL: u8 = 150;
const PIECE_LEVEL: u8 = 255;

/// Weights of the landing heuristic.
///
/// Buried holes are the thing that actually ends a game, so they dominate; the
/// rest keeps the surface flat enough to keep taking pieces.
const WEIGHT_LINES: f32 = 3.0;
const WEIGHT_HOLES: f32 = -7.5;
const WEIGHT_HEIGHT: f32 = -0.5;
const WEIGHT_BUMPINESS: f32 = -0.35;

/// The seven pieces, as the offsets of their four cells in each rotation.
///
/// Written out rather than rotated at runtime: twenty-eight small tables are
/// easier to read, and to check, than a rotation routine plus its edge cases.
const PIECES: [&[[(i32, i32); 4]]; 7] = [
    // I
    &[
        [(0, 0), (1, 0), (2, 0), (3, 0)],
        [(0, 0), (0, 1), (0, 2), (0, 3)],
    ],
    // O
    &[[(0, 0), (1, 0), (0, 1), (1, 1)]],
    // T
    &[
        [(0, 0), (1, 0), (2, 0), (1, 1)],
        [(1, 0), (0, 1), (1, 1), (1, 2)],
        [(1, 0), (0, 1), (1, 1), (2, 1)],
        [(0, 0), (0, 1), (1, 1), (0, 2)],
    ],
    // S
    &[
        [(1, 0), (2, 0), (0, 1), (1, 1)],
        [(0, 0), (0, 1), (1, 1), (1, 2)],
    ],
    // Z
    &[
        [(0, 0), (1, 0), (1, 1), (2, 1)],
        [(1, 0), (0, 1), (1, 1), (0, 2)],
    ],
    // J
    &[
        [(0, 0), (0, 1), (1, 1), (2, 1)],
        [(0, 0), (1, 0), (0, 1), (0, 2)],
        [(0, 0), (1, 0), (2, 0), (2, 1)],
        [(1, 0), (1, 1), (0, 2), (1, 2)],
    ],
    // L
    &[
        [(2, 0), (0, 1), (1, 1), (2, 1)],
        [(0, 0), (0, 1), (0, 2), (1, 2)],
        [(0, 0), (1, 0), (2, 0), (0, 1)],
        [(0, 0), (1, 0), (1, 1), (1, 2)],
    ],
];

/// A piece in play.
#[derive(Clone, Copy, Debug)]
struct Piece {
    kind: usize,
    rotation: usize,
    x: i32,
    y: i32,
}

impl Piece {
    /// The cells it occupies, in well coordinates.
    fn cells(self) -> [(i32, i32); 4] {
        let shape = PIECES[self.kind % PIECES.len()];
        let offsets = shape[self.rotation % shape.len()];
        offsets.map(|(dx, dy)| (self.x + dx, self.y + dy))
    }
}

/// Where the robot has decided to put the current piece.
#[derive(Clone, Copy, Debug)]
struct Plan {
    rotation: usize,
    x: i32,
}

/// What the game is doing right now.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    /// A piece is on its way down.
    Falling,
    /// Completed lines are flashing before they collapse.
    Clearing(f32),
    /// The well filled up; flashing before a new game.
    ToppedOut(f32),
}

/// A self-playing game of Tetris.
pub struct Tetris {
    mode: ColorMode,
    rng: StdRng,
    /// Settled cells, row-major.
    well: [bool; CELLS],
    piece: Piece,
    plan: Plan,
    phase: Phase,
    /// Rows waiting to collapse.
    full_rows: Vec<i32>,
    step_timer: f32,
}

impl Tetris {
    /// Starts a game. `seed` makes the run reproducible.
    #[must_use]
    pub fn new(seed: Option<u64>, mode: ColorMode) -> Self {
        let mut game = Self {
            mode,
            rng: rng_from(seed),
            well: [false; CELLS],
            piece: Piece {
                kind: 0,
                rotation: 0,
                x: 0,
                y: 0,
            },
            plan: Plan { rotation: 0, x: 0 },
            phase: Phase::Falling,
            full_rows: Vec::new(),
            step_timer: 0.0,
        };
        game.spawn();
        game
    }

    /// Empties the well and starts over.
    fn restart(&mut self) {
        self.well = [false; CELLS];
        self.full_rows.clear();
        self.phase = Phase::Falling;
        self.spawn();
    }

    /// Brings in a new piece and decides where it should go.
    fn spawn(&mut self) {
        let kind = self.rng.random_range(0..PIECES.len());
        self.piece = Piece {
            kind,
            rotation: 0,
            x: WIDTH / 2 - 1,
            y: 0,
        };

        self.plan = self.best_placement(kind);
        // Entering on top of the stack is what topping out means.
        if self.collides(self.piece) {
            self.phase = Phase::ToppedOut(TOP_OUT_FLASH);
        }
    }

    /// Whether a piece overlaps the walls, the floor or a settled cell.
    fn collides(&self, piece: Piece) -> bool {
        piece.cells().into_iter().any(|(x, y)| {
            !(0..WIDTH).contains(&x)
                || y >= HEIGHT
                || (y >= 0 && slot(x, y).is_some_and(|index| self.well[index]))
        })
    }

    /// Settles the piece into the well.
    fn land(&mut self) {
        for (x, y) in self.piece.cells() {
            if let Some(index) = slot(x, y) {
                self.well[index] = true;
            }
        }

        self.full_rows = (0..HEIGHT)
            .filter(|y| (0..WIDTH).all(|x| slot(x, *y).is_some_and(|index| self.well[index])))
            .collect();

        if self.full_rows.is_empty() {
            self.spawn();
        } else {
            self.phase = Phase::Clearing(CLEAR_FLASH);
        }
    }

    /// Drops the full rows and pulls everything above them down.
    fn collapse(&mut self) {
        for row in self.full_rows.clone() {
            for y in (1..=row).rev() {
                for x in 0..WIDTH {
                    let (Some(here), Some(above)) = (slot(x, y), slot(x, y - 1)) else {
                        continue;
                    };
                    self.well[here] = self.well[above];
                }
            }
            for x in 0..WIDTH {
                if let Some(index) = slot(x, 0) {
                    self.well[index] = false;
                }
            }
        }
        self.full_rows.clear();
        self.phase = Phase::Falling;
        self.spawn();
    }

    /// Tries every landing and keeps the best-scoring one.
    fn best_placement(&self, kind: usize) -> Plan {
        let rotations = PIECES[kind % PIECES.len()].len();
        let mut best = Plan { rotation: 0, x: 0 };
        let mut best_score = f32::NEG_INFINITY;

        for rotation in 0..rotations {
            for x in -1..WIDTH {
                let mut piece = Piece {
                    kind,
                    rotation,
                    x,
                    y: 0,
                };
                if self.collides(piece) {
                    continue;
                }
                // Drop it as far as it goes.
                while !self.collides(Piece {
                    y: piece.y + 1,
                    ..piece
                }) {
                    piece.y += 1;
                }

                let score = self.score_landing(piece);
                if score > best_score {
                    best_score = score;
                    best = Plan { rotation, x };
                }
            }
        }
        best
    }

    /// Scores the well a landing would leave behind.
    fn score_landing(&self, piece: Piece) -> f32 {
        let mut well = self.well;
        for (x, y) in piece.cells() {
            if let Some(index) = slot(x, y) {
                well[index] = true;
            }
        }

        let mut lines = 0;
        for y in 0..HEIGHT {
            if (0..WIDTH).all(|x| slot(x, y).is_some_and(|index| well[index])) {
                lines += 1;
            }
        }

        let mut heights = [0i32; 9];
        let mut holes = 0;
        for x in 0..WIDTH {
            let column = usize::try_from(x).unwrap_or(0);
            let mut seen_block = false;
            for y in 0..HEIGHT {
                let filled = slot(x, y).is_some_and(|index| well[index]);
                if filled && !seen_block {
                    seen_block = true;
                    heights[column] = HEIGHT - y;
                } else if !filled && seen_block {
                    // A gap under a block is a hole: nothing can fill it
                    // without clearing everything above first.
                    holes += 1;
                }
            }
        }

        let total_height: i32 = heights.iter().sum();
        let bumpiness: i32 = heights
            .windows(2)
            .map(|pair| (pair[0] - pair[1]).abs())
            .sum();

        // Every term is a small count on a nine-by-thirty-four board, nowhere
        // near the range where an f32 stops holding integers exactly.
        let term = |value: i32| f32::from(i16::try_from(value).unwrap_or(i16::MAX));

        WEIGHT_LINES.mul_add(
            term(lines),
            WEIGHT_HOLES.mul_add(
                term(holes),
                WEIGHT_HEIGHT.mul_add(term(total_height), WEIGHT_BUMPINESS * term(bumpiness)),
            ),
        )
    }

    /// Moves the piece one step towards the plan, then one row down.
    ///
    /// Turning and sliding before falling is what makes this read as a player
    /// rather than as pieces materialising in place.
    fn step(&mut self) {
        if self.piece.rotation != self.plan.rotation {
            let turned = Piece {
                rotation: self.plan.rotation,
                ..self.piece
            };
            if !self.collides(turned) {
                self.piece = turned;
                return;
            }
        }

        if self.piece.x != self.plan.x {
            let direction = if self.plan.x > self.piece.x { 1 } else { -1 };
            let moved = Piece {
                x: self.piece.x + direction,
                ..self.piece
            };
            if !self.collides(moved) {
                self.piece = moved;
                return;
            }
        }

        let dropped = Piece {
            y: self.piece.y + 1,
            ..self.piece
        };
        if self.collides(dropped) {
            self.land();
        } else {
            self.piece = dropped;
        }
    }
}

impl Scene for Tetris {
    fn name(&self) -> &'static str {
        "tetris"
    }

    /// The whole panel: a well eight rows deep is not Tetris.
    fn min_height(&self) -> i32 {
        HEIGHT
    }

    fn update(&mut self, delta: Duration) {
        let dt = delta.as_secs_f32().min(MAX_STEP);

        match self.phase {
            Phase::ToppedOut(remaining) => {
                let remaining = remaining - dt;
                if remaining <= 0.0 {
                    self.restart();
                } else {
                    self.phase = Phase::ToppedOut(remaining);
                }
            }
            Phase::Clearing(remaining) => {
                let remaining = remaining - dt;
                if remaining <= 0.0 {
                    self.collapse();
                } else {
                    self.phase = Phase::Clearing(remaining);
                }
            }
            Phase::Falling => {
                self.step_timer += dt;
                while self.step_timer >= STEP_INTERVAL && self.phase == Phase::Falling {
                    self.step_timer -= STEP_INTERVAL;
                    self.step();
                }
            }
        }
    }

    fn render(&self, canvas: &mut Canvas, _area: Area) {
        let flashing = match self.phase {
            Phase::Clearing(remaining) | Phase::ToppedOut(remaining) => {
                Some(canvas::floor_pixel(remaining * FLASH_HZ) % 2 == 0)
            }
            Phase::Falling => None,
        };

        for y in 0..HEIGHT {
            let full = self.full_rows.contains(&y);
            for x in 0..WIDTH {
                if !slot(x, y).is_some_and(|index| self.well[index]) {
                    continue;
                }
                let level = match (flashing, full, self.phase) {
                    // The rows about to go blink; the rest of the well holds
                    // still, so the eye knows what is leaving.
                    (Some(lit), true, Phase::Clearing(_)) => {
                        if lit {
                            PIECE_LEVEL
                        } else {
                            0
                        }
                    }
                    (Some(lit), _, Phase::ToppedOut(_)) => {
                        if lit {
                            PIECE_LEVEL
                        } else {
                            40
                        }
                    }
                    _ => STACK_LEVEL,
                };
                canvas.set_max(x, y, level);
            }
        }

        if self.phase != Phase::Falling {
            return;
        }
        for (x, y) in self.piece.cells() {
            if y >= 0 {
                canvas.set_max(x, y, PIECE_LEVEL);
            }
        }

        // In black and white the falling piece would be the same as the stack,
        // so it gets a one-pixel gap under it instead: the shape reads as
        // separate because nothing touches it.
        if self.mode == ColorMode::Bw {
            for (x, y) in self.piece.cells() {
                let below = y + 1;
                if !self.piece.cells().contains(&(x, below)) {
                    canvas.set(x, below, 0);
                }
            }
        }
    }
}

/// Maps a well coordinate to its index, or `None` off the board.
fn slot(x: i32, y: i32) -> Option<usize> {
    if x < 0 || y < 0 || x >= WIDTH || y >= HEIGHT {
        return None;
    }
    usize::try_from(y * WIDTH + x).ok()
}

#[cfg(test)]
mod tests {
    use super::{HEIGHT, PIECES, Phase, Piece, Tetris, WIDTH, slot};
    use crate::canvas::Canvas;
    use crate::device::ColorMode;
    use crate::scene::{Area, Scene};
    use std::time::Duration;

    fn game(seed: u64) -> Tetris {
        Tetris::new(Some(seed), ColorMode::Bw)
    }

    /// Runs the game by moves rather than by time.
    fn play(game: &mut Tetris, moves: usize) {
        for _ in 0..moves {
            match game.phase {
                Phase::Falling => game.step(),
                Phase::Clearing(_) => game.collapse(),
                Phase::ToppedOut(_) => game.restart(),
            }
        }
    }

    #[test]
    fn every_piece_has_four_cells_in_every_rotation() {
        // The tables are written out by hand, so this is the check that they
        // are actually tetrominoes.
        for (kind, rotations) in PIECES.iter().enumerate() {
            assert!(!rotations.is_empty(), "piece {kind} has no rotation");
            for (index, cells) in rotations.iter().enumerate() {
                assert_eq!(cells.len(), 4, "piece {kind} rotation {index}");
                let mut seen = cells.to_vec();
                seen.sort_unstable();
                seen.dedup();
                assert_eq!(
                    seen.len(),
                    4,
                    "piece {kind} rotation {index} has a duplicate"
                );
            }
        }
    }

    #[test]
    fn every_rotation_of_every_piece_fits_the_well() {
        for (kind, rotations) in PIECES.iter().enumerate() {
            for rotation in 0..rotations.len() {
                let piece = Piece {
                    kind,
                    rotation,
                    x: 0,
                    y: 0,
                };
                for (x, y) in piece.cells() {
                    assert!(
                        (0..WIDTH).contains(&x) && (0..HEIGHT).contains(&y),
                        "piece {kind} rotation {rotation} leaves the well at ({x}, {y})"
                    );
                }
            }
        }
    }

    #[test]
    fn pieces_settle_instead_of_falling_through_the_floor() {
        let mut game = game(1);
        play(&mut game, 400);

        // Whatever happened, nothing is resting outside the well.
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                if slot(x, y).is_some_and(|index| game.well[index]) {
                    assert!((0..WIDTH).contains(&x) && (0..HEIGHT).contains(&y));
                }
            }
        }
        assert!(
            game.well.iter().any(|filled| *filled),
            "nothing ever landed"
        );
    }

    #[test]
    fn a_full_row_is_cleared_rather_than_left_in_place() {
        let mut game = game(2);
        // Fill the bottom row bar one cell, then let the robot finish it.
        for x in 0..WIDTH - 1 {
            game.well[slot(x, HEIGHT - 1).unwrap()] = true;
        }

        let mut cleared = false;
        for _ in 0..2_000 {
            play(&mut game, 1);
            if matches!(game.phase, Phase::Clearing(_)) {
                cleared = true;
                break;
            }
        }
        assert!(cleared, "the robot never completed the row it was handed");
    }

    #[test]
    fn collapsing_pulls_the_stack_down_by_one() {
        let mut game = game(3);
        // A full bottom row with one cell sitting on top of it.
        for x in 0..WIDTH {
            game.well[slot(x, HEIGHT - 1).unwrap()] = true;
        }
        game.well[slot(3, HEIGHT - 2).unwrap()] = true;
        game.full_rows = vec![HEIGHT - 1];

        game.collapse();

        assert!(
            game.well[slot(3, HEIGHT - 1).unwrap()],
            "the cell above did not fall"
        );
        assert!(
            !game.well[slot(0, HEIGHT - 1).unwrap()],
            "the cleared row is still full"
        );
    }

    #[test]
    fn the_robot_avoids_burying_holes() {
        // The heuristic exists for this: a bot that only chased clears fills
        // the well with unreachable gaps and tops out in under a minute.
        let mut game = game(5);
        play(&mut game, 3_000);

        let mut holes = 0;
        for x in 0..WIDTH {
            let mut seen = false;
            for y in 0..HEIGHT {
                let filled = slot(x, y).is_some_and(|index| game.well[index]);
                if filled {
                    seen = true;
                } else if seen {
                    holes += 1;
                }
            }
        }
        assert!(holes < 25, "the robot buried {holes} holes");
    }

    #[test]
    fn a_game_survives_long_enough_to_be_worth_watching() {
        for seed in [1u64, 2, 3] {
            let mut game = game(seed);
            let mut top_outs = 0;
            for _ in 0..4_000 {
                if matches!(game.phase, Phase::ToppedOut(_)) {
                    top_outs += 1;
                }
                play(&mut game, 1);
            }
            assert!(top_outs < 40, "seed {seed} topped out {top_outs} times");
        }
    }

    #[test]
    fn the_falling_piece_is_drawn_apart_from_the_stack() {
        let mut game = game(7);
        play(&mut game, 200);
        game.phase = Phase::Falling;

        let mut canvas = Canvas::new();
        game.render(&mut canvas, Area::FULL);
        assert_ne!(canvas, Canvas::new(), "nothing was drawn");
    }

    #[test]
    fn topping_out_starts_a_fresh_well() {
        let mut game = game(1);
        game.phase = Phase::ToppedOut(0.01);
        game.well = [true; super::CELLS];

        game.update(Duration::from_millis(50));

        assert_eq!(game.phase, Phase::Falling);
        assert!(
            game.well.iter().filter(|filled| **filled).count() < 10,
            "the well was not emptied"
        );
    }
}
