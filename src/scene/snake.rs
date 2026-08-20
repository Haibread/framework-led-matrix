//! Snake, played by a robot that refuses to trap itself.
//!
//! Chasing the food with a shortest path alone is what kills naive snake bots:
//! they eat their way into a pocket and suffocate a few seconds later. This one
//! plans the whole path to the food, simulates itself walking it, and only
//! commits if it can still reach its own tail afterwards — because a snake that
//! can reach its tail always has a way out. When no safe path exists it follows
//! its tail and waits for the board to open up.

use std::collections::VecDeque;
use std::time::Duration;

use rand::RngExt;
use rand::rngs::StdRng;

use crate::canvas::{self, Canvas, PIXELS};
use crate::device::ColorMode;
use crate::scene::{Area, Scene, rng_from};

/// Time between two moves.
///
/// Deliberately unhurried. On a nine-wide one-bit panel the head is just a lit
/// pixel like any other, so the only thing telling you where the snake is going
/// is continuity of motion — and at the 110 ms this used to be, the eye keeps
/// losing it the moment the head passes close to the body.
const STEP_INTERVAL: f32 = 0.16;
/// How long the death animation lasts before a new game starts.
const DEATH_FLASH: f32 = 1.1;
/// Longest simulated step, so a stalled thread does not fast-forward the game.
const MAX_STEP: f32 = 0.1;

/// Moves allowed without eating before the run is declared stuck.
const STARVATION_LIMIT: u32 = 900;

/// Length at which the run is declared won and a fresh one starts.
///
/// Not an arbitrary difficulty knob — it is what keeps the picture readable. The
/// head is just a lit pixel among others, so the only way to follow it is
/// continuity, and that breaks down as soon as the head runs alongside the body.
/// Measured over 1.6M moves, how often that happens climbs with length: 1% below
/// ten cells, 5% in the teens, 10% in the twenties, 16% in the thirties. Past
/// that the snake reads as a tangle passing through itself, however correct it
/// is. Forty is the point where a run still resolves in about a minute and a
/// half and the tangle is not yet the dominant impression.
const MAX_LENGTH: usize = 40;

const HEAD_LEVEL: u8 = 255;

/// Rate and duty cycle of the food's twinkle in black and white.
///
/// The body is never blinked, only the food, and even then it stays lit most of
/// the time. Blinking a pixel that is part of the snake costs more than it buys:
/// the head used to blink at 7 Hz to mark it, which hid it for longer than the
/// 110 ms it takes to move, so it kept reappearing a cell or two away — on a
/// coiled snake that reads as the head walking through its own body. Direction
/// is legible from movement; a strobing body is not legible at all.
const FOOD_BLINK_HZ: f32 = 1.5;
const FOOD_DUTY: f32 = 0.75;

/// Toggles per second of the death flash. Slow enough to read as an event
/// rather than as a fault.
const DEATH_FLASH_HZ: f32 = 5.0;
/// How long the win animation lasts before a new game starts.
///
/// A win used to flash the body like a death did, only slower — 2 Hz against
/// 5 Hz. On a one-bit panel that is not a difference anyone can see, so finishing
/// a run read as dying, which was reported as the snake killing itself once it
/// got long. The win now retracts the body into the head instead: every segment
/// left is steadily lit, nothing ever blinks, and the two endings can no longer
/// be confused.
const WIN_RETRACT: f32 = 1.4;
const BODY_BRIGHTEST: u32 = 190;
const BODY_DIMMEST: u32 = 45;

/// Length the snake starts with.
const START_LENGTH: i32 = 3;

/// A square on the board.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cell {
    x: i32,
    y: i32,
}

impl Cell {
    /// The neighbouring cell in `direction`.
    fn shifted(self, direction: Direction) -> Self {
        let (dx, dy) = direction.delta();
        Self {
            x: self.x + dx,
            y: self.y + dy,
        }
    }
}

/// One of the four moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
    Up,
    Down,
    Left,
    Right,
}

impl Direction {
    const ALL: [Self; 4] = [Self::Up, Self::Down, Self::Left, Self::Right];

    fn delta(self) -> (i32, i32) {
        match self {
            Self::Up => (0, -1),
            Self::Down => (0, 1),
            Self::Left => (-1, 0),
            Self::Right => (1, 0),
        }
    }
}

/// What the game is doing right now.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    /// The snake is alive and moving.
    Playing,
    /// The snake died; flashing before the next game.
    Dying(f32),
    /// The snake reached [`MAX_LENGTH`]; retracting before the next game.
    Won(f32),
}

/// A self-playing game of Snake.
pub struct Snake {
    mode: ColorMode,
    rng: StdRng,
    /// The snake, head first.
    body: VecDeque<Cell>,
    /// Mirror of `body` for O(1) collision tests.
    occupied: [bool; PIXELS],
    food: Cell,
    phase: Phase,
    step_timer: f32,
    elapsed: f32,
    /// Moves since the last meal.
    hunger: u32,
}

impl Snake {
    /// Starts a game. `seed` makes the run reproducible.
    #[must_use]
    pub fn new(seed: Option<u64>, mode: ColorMode) -> Self {
        let mut snake = Self {
            mode,
            rng: rng_from(seed),
            body: VecDeque::new(),
            occupied: [false; PIXELS],
            food: Cell { x: 4, y: 8 },
            phase: Phase::Playing,
            step_timer: 0.0,
            elapsed: 0.0,
            hunger: 0,
        };
        snake.restart();
        snake
    }

    /// Resets the board to a fresh game.
    fn restart(&mut self) {
        self.body.clear();
        self.occupied = [false; PIXELS];

        // A short vertical snake in the middle, head pointing up.
        for offset in 0..START_LENGTH {
            let cell = Cell {
                x: canvas::WIDTH / 2,
                y: canvas::HEIGHT / 2 + offset,
            };
            self.body.push_back(cell);
            if let Some(index) = slot(cell) {
                self.occupied[index] = true;
            }
        }

        self.hunger = 0;
        self.step_timer = 0.0;
        self.phase = Phase::Playing;
        self.place_food();
    }

    /// The cell the head is on.
    fn head(&self) -> Cell {
        self.body.front().copied().unwrap_or(Cell { x: 0, y: 0 })
    }

    /// Drops food on a random free square.
    fn place_food(&mut self) {
        let mut free = Vec::with_capacity(PIXELS);
        for y in 0..canvas::HEIGHT {
            for x in 0..canvas::WIDTH {
                let cell = Cell { x, y };
                if slot(cell).is_some_and(|index| !self.occupied[index]) {
                    free.push(cell);
                }
            }
        }

        if free.is_empty() {
            // The board is full: a perfect game. Celebrate, then start over.
            self.phase = Phase::Won(WIN_RETRACT);
            return;
        }

        let pick = self.rng.random_range(0..free.len());
        self.food = free[pick];
    }

    /// Whether the snake may move into `cell` this step.
    ///
    /// `tail_vacates` is false when the snake is about to grow, because then
    /// the tail stays where it is instead of moving out of the way.
    fn is_free(&self, cell: Cell, tail_vacates: bool) -> bool {
        let Some(index) = slot(cell) else {
            return false;
        };
        if !self.occupied[index] {
            return true;
        }
        tail_vacates && self.body.back().copied() == Some(cell)
    }

    /// Advances the game by one move.
    fn step(&mut self) {
        let Some(direction) = self.choose_move() else {
            self.die();
            return;
        };

        let next = self.head().shifted(direction);
        let eats = next == self.food;

        if !self.is_free(next, !eats) {
            self.die();
            return;
        }

        if !eats {
            if let Some(tail) = self.body.pop_back() {
                if let Some(index) = slot(tail) {
                    self.occupied[index] = false;
                }
            }
        }

        self.body.push_front(next);
        if let Some(index) = slot(next) {
            self.occupied[index] = true;
        }

        if eats {
            self.hunger = 0;
            if self.body.len() >= MAX_LENGTH {
                self.phase = Phase::Won(WIN_RETRACT);
                return;
            }
            self.place_food();
        } else {
            self.hunger += 1;
            if self.hunger > STARVATION_LIMIT {
                self.die();
            }
        }
    }

    /// Ends the run and starts the death animation.
    fn die(&mut self) {
        self.phase = Phase::Dying(DEATH_FLASH);
    }

    /// Picks the next move, preferring a safe path to the food.
    fn choose_move(&self) -> Option<Direction> {
        self.safe_path_to_food()
            .or_else(|| self.follow_tail())
            .or_else(|| self.most_open_move())
    }

    /// The best safe move toward the food.
    ///
    /// Ranked by safety, then by path length, then by how much the move would
    /// run alongside the snake's own body. That last term is not strategy for
    /// its own sake: on a one-bit panel a head sitting against the body merges
    /// into it, and when it emerges a few cells later it reads as having gone
    /// straight through. Measured on a long run, the head used to end up against
    /// its own body on 17% of moves. Avoiding it also happens to be better play,
    /// since hugging yourself closes off space.
    fn safe_path_to_food(&self) -> Option<Direction> {
        let head = self.head();
        let mut blocked = self.occupied;
        // By the time the snake gets there, the tail will have moved on.
        if let Some(index) = self.body.back().copied().and_then(slot) {
            blocked[index] = false;
        }

        let mut best: Option<(usize, usize, Direction)> = None;

        for direction in Direction::ALL {
            let next = head.shifted(direction);
            let eats = next == self.food;
            if !self.is_free(next, !eats) {
                continue;
            }

            // The whole path this move commits to, so the safety check below
            // judges the same thing the snake would actually walk.
            let mut path = vec![next];
            if !eats {
                let Some(rest) = search(next, self.food, &blocked) else {
                    continue;
                };
                path.extend(rest);
            }

            if !self.survives(&path) {
                continue;
            }

            let candidate = (path.len(), self.hugs(next), direction);
            if best.is_none_or(|(length, hugs, _)| (candidate.0, candidate.1) < (length, hugs)) {
                best = Some(candidate);
            }
        }

        best.map(|(_, _, direction)| direction)
    }

    /// How many of its own segments the snake would sit against at `cell`.
    ///
    /// The neck does not count: the head is always against that one.
    fn hugs(&self, cell: Cell) -> usize {
        self.hugs_excluding_neck(cell, self.head())
    }

    /// Segments adjacent to `cell`, ignoring `neck`.
    fn hugs_excluding_neck(&self, cell: Cell, neck: Cell) -> usize {
        Direction::ALL
            .into_iter()
            .map(|direction| cell.shifted(direction))
            .filter(|neighbour| *neighbour != neck)
            .filter(|neighbour| slot(*neighbour).is_some_and(|index| self.occupied[index]))
            .count()
    }

    /// Whether the snake can still reach its tail after walking `path`.
    fn survives(&self, path: &[Cell]) -> bool {
        let mut body = self.body.clone();
        let mut occupied = self.occupied;

        for (index, cell) in path.iter().enumerate() {
            // Only the last cell of the path is the food, so only it grows us.
            let eats = index == path.len() - 1;
            if !eats {
                if let Some(tail) = body.pop_back() {
                    if let Some(slot_index) = slot(tail) {
                        occupied[slot_index] = false;
                    }
                }
            }
            body.push_front(*cell);
            if let Some(slot_index) = slot(*cell) {
                occupied[slot_index] = true;
            }
        }

        let (Some(head), Some(tail)) = (body.front().copied(), body.back().copied()) else {
            return false;
        };
        if head == tail {
            return true;
        }

        let mut blocked = occupied;
        if let Some(index) = slot(tail) {
            blocked[index] = false;
        }
        search(head, tail, &blocked).is_some()
    }

    /// Chases its own tail, which is always safe and buys time.
    fn follow_tail(&self) -> Option<Direction> {
        let tail = self.body.back().copied()?;
        let mut blocked = self.occupied;
        if let Some(index) = slot(tail) {
            blocked[index] = false;
        }

        let path = search(self.head(), tail, &blocked)?;
        direction_between(self.head(), *path.first()?)
    }

    /// Last resort: step into whichever neighbour has the most room behind it.
    fn most_open_move(&self) -> Option<Direction> {
        let head = self.head();
        Direction::ALL
            .into_iter()
            .filter(|direction| self.is_free(head.shifted(*direction), true))
            .max_by_key(|direction| self.open_space(head.shifted(*direction)))
    }

    /// Draws the `shown` segments closest to the head, at playing brightness.
    ///
    /// The gradient is always keyed on the full length, so a body being
    /// retracted keeps the brightness it had while playing instead of
    /// rescaling itself a little brighter on every frame.
    fn render_body(&self, canvas: &mut Canvas, shown: usize) {
        let length = self.body.len();
        for (index, cell) in self.body.iter().take(shown).enumerate() {
            let level = if index == 0 {
                HEAD_LEVEL
            } else {
                body_level(index, length)
            };
            canvas.set_max(cell.x, cell.y, level);
        }
    }

    /// Number of squares reachable from `from`, ignoring the tail's movement.
    fn open_space(&self, from: Cell) -> usize {
        let mut seen = [false; PIXELS];
        let mut queue = VecDeque::new();
        let Some(start) = slot(from) else {
            return 0;
        };
        if self.occupied[start] {
            return 0;
        }

        seen[start] = true;
        queue.push_back(from);
        let mut count = 0;

        while let Some(cell) = queue.pop_front() {
            count += 1;
            for direction in Direction::ALL {
                let next = cell.shifted(direction);
                let Some(index) = slot(next) else {
                    continue;
                };
                if seen[index] || self.occupied[index] {
                    continue;
                }
                seen[index] = true;
                queue.push_back(next);
            }
        }

        count
    }
}

impl Scene for Snake {
    fn name(&self) -> &'static str {
        "snake"
    }

    /// The whole panel. A game played on a strip is not the same game, so this
    /// asks for everything rather than degrading.
    fn min_height(&self) -> i32 {
        canvas::HEIGHT
    }

    fn update(&mut self, delta: Duration) {
        let dt = delta.as_secs_f32().min(MAX_STEP);
        self.elapsed += dt;

        match self.phase {
            Phase::Dying(remaining) => {
                let remaining = remaining - dt;
                if remaining <= 0.0 {
                    self.restart();
                } else {
                    self.phase = Phase::Dying(remaining);
                }
            }
            Phase::Won(remaining) => {
                let remaining = remaining - dt;
                if remaining <= 0.0 {
                    self.restart();
                } else {
                    self.phase = Phase::Won(remaining);
                }
            }
            Phase::Playing => {
                self.step_timer += dt;
                while self.step_timer >= STEP_INTERVAL && self.phase == Phase::Playing {
                    self.step_timer -= STEP_INTERVAL;
                    self.step();
                }
            }
        }
    }

    fn render(&self, canvas: &mut Canvas, _area: Area) {
        match self.phase {
            Phase::Dying(remaining) => {
                // Deliberate blinking, unlike anything else this scene draws:
                // this one is announcing that the run ended badly.
                let lit = canvas::floor_pixel(remaining * DEATH_FLASH_HZ) % 2 == 0;
                let level = if lit { 210 } else { 30 };
                for cell in &self.body {
                    canvas.set_max(cell.x, cell.y, level);
                }
                return;
            }
            Phase::Won(remaining) => {
                // The body swallows itself tail first, holding its playing
                // brightness the whole way so nothing ever blinks. See
                // [`WIN_RETRACT`] for why a win must not look like a death.
                self.render_body(canvas, retracted_length(self.body.len(), remaining));
                return;
            }
            Phase::Playing => {}
        }

        // The food has to read differently from the body. Greyscale can pulse
        // it; black and white only has on and off, so it twinkles — mostly lit,
        // with a short blink.
        let food = match self.mode {
            ColorMode::Greyscale => {
                canvas::level(0.45f32.mul_add((self.elapsed * 6.0).sin(), 0.55))
            }
            ColorMode::Bw if blinking(self.elapsed, FOOD_BLINK_HZ, FOOD_DUTY) => u8::MAX,
            ColorMode::Bw => 0,
        };
        canvas.set_max(self.food.x, self.food.y, food);

        // Every segment stays lit, in both modes. Greyscale marks the head by
        // brightness; black and white does not mark it at all, on purpose. See
        // [`FOOD_BLINK_HZ`] for why nothing on the snake may blink.
        self.render_body(canvas, self.body.len());
    }
}

/// How many segments are still lit `remaining` seconds into the win animation.
///
/// Counts down to zero over [`WIN_RETRACT`] whatever the length, so the ending
/// takes the same time on a snake that filled the board as on one that only
/// just reached [`MAX_LENGTH`].
fn retracted_length(length: usize, remaining: f32) -> usize {
    // Turned into a per-mille integer first, so the count is exact arithmetic
    // on the length rather than a float scaled by it.
    let fraction = (remaining / WIN_RETRACT).clamp(0.0, 1.0);
    let permille = u32::try_from(canvas::to_pixel(fraction * 1000.0)).unwrap_or(0);
    let total = u32::try_from(length).unwrap_or(u32::MAX);
    usize::try_from(total * permille / 1000).unwrap_or(0)
}

/// Whether something blinking at `hz` is lit, `duty` being the lit share of
/// each cycle.
fn blinking(elapsed: f32, hz: f32, duty: f32) -> bool {
    let phase = (elapsed * hz).rem_euclid(1.0);
    phase < duty
}

/// Maps a cell to its index in the occupancy grid.
fn slot(cell: Cell) -> Option<usize> {
    if cell.x < 0 || cell.y < 0 || cell.x >= canvas::WIDTH || cell.y >= canvas::HEIGHT {
        return None;
    }
    usize::try_from(cell.y * canvas::WIDTH + cell.x).ok()
}

/// The direction taking you from `from` to an adjacent `to`.
fn direction_between(from: Cell, to: Cell) -> Option<Direction> {
    Direction::ALL
        .into_iter()
        .find(|direction| from.shifted(*direction) == to)
}

/// Breadth-first search for the shortest path from `from` to `to`.
///
/// Returns the cells to walk, excluding `from` and including `to`, or `None`
/// when `to` is unreachable. The destination itself may be marked blocked —
/// that is how the snake targets its own tail.
fn search(from: Cell, to: Cell, blocked: &[bool; PIXELS]) -> Option<Vec<Cell>> {
    let start = slot(from)?;
    slot(to)?;

    let mut seen = [false; PIXELS];
    let mut previous: [Option<Cell>; PIXELS] = [None; PIXELS];
    let mut queue = VecDeque::new();

    seen[start] = true;
    queue.push_back(from);

    while let Some(cell) = queue.pop_front() {
        if cell == to {
            return Some(reconstruct(&previous, from, to));
        }
        for direction in Direction::ALL {
            let next = cell.shifted(direction);
            let Some(index) = slot(next) else {
                continue;
            };
            if seen[index] || (blocked[index] && next != to) {
                continue;
            }
            seen[index] = true;
            previous[index] = Some(cell);
            queue.push_back(next);
        }
    }

    None
}

/// Walks the predecessor table backwards into a forward path.
fn reconstruct(previous: &[Option<Cell>; PIXELS], from: Cell, to: Cell) -> Vec<Cell> {
    let mut path = Vec::new();
    let mut cursor = to;
    while cursor != from {
        path.push(cursor);
        let Some(index) = slot(cursor) else {
            break;
        };
        let Some(parent) = previous[index] else {
            break;
        };
        cursor = parent;
    }
    path.reverse();
    path
}

/// Brightness for a body segment, fading from just behind the head to the tail.
fn body_level(index: usize, length: usize) -> u8 {
    let span = BODY_BRIGHTEST - BODY_DIMMEST;
    let total = u32::try_from(length.max(1)).unwrap_or(1);
    let remaining = u32::try_from(length.saturating_sub(index)).unwrap_or(0);
    let value = BODY_DIMMEST + span * remaining.min(total) / total;
    u8::try_from(value).unwrap_or(u8::MAX)
}

#[cfg(test)]
mod tests {
    use super::{
        Cell, Direction, PIXELS, Phase, Snake, WIN_RETRACT, body_level, retracted_length, search,
        slot,
    };
    use crate::canvas::{self, Canvas};
    use crate::device::{BW_THRESHOLD, ColorMode};
    use crate::scene::{Area, Scene};
    use std::time::Duration;

    fn empty_grid() -> [bool; PIXELS] {
        [false; PIXELS]
    }

    #[test]
    fn search_finds_a_straight_path() {
        let path = search(Cell { x: 0, y: 0 }, Cell { x: 0, y: 3 }, &empty_grid())
            .expect("path across an empty board");
        assert_eq!(path.len(), 3, "path excludes the starting cell");
        assert_eq!(path[2], Cell { x: 0, y: 3 });
    }

    #[test]
    fn search_gives_up_when_walled_in() {
        let mut blocked = empty_grid();
        // Fence off the top-left corner.
        for cell in [Cell { x: 1, y: 0 }, Cell { x: 0, y: 1 }] {
            blocked[slot(cell).unwrap()] = true;
        }
        assert!(search(Cell { x: 0, y: 0 }, Cell { x: 5, y: 5 }, &blocked).is_none());
    }

    #[test]
    fn search_may_target_a_blocked_destination() {
        let mut blocked = empty_grid();
        let tail = Cell { x: 0, y: 3 };
        blocked[slot(tail).unwrap()] = true;
        assert!(search(Cell { x: 0, y: 0 }, tail, &blocked).is_some());
    }

    #[test]
    fn slot_rejects_cells_off_the_board() {
        assert!(slot(Cell { x: -1, y: 0 }).is_none());
        assert!(slot(Cell { x: 0, y: -1 }).is_none());
        assert!(
            slot(Cell {
                x: canvas::WIDTH,
                y: 0
            })
            .is_none()
        );
        assert!(
            slot(Cell {
                x: 0,
                y: canvas::HEIGHT
            })
            .is_none()
        );
        assert_eq!(slot(Cell { x: 8, y: 33 }), Some(PIXELS - 1));
    }

    #[test]
    fn a_new_game_starts_alive_with_food_off_the_body() {
        let snake = Snake::new(Some(1), ColorMode::Bw);
        assert_eq!(snake.phase, Phase::Playing);
        assert_eq!(snake.body.len(), 3);
        assert!(!snake.body.contains(&snake.food));
    }

    #[test]
    fn the_body_and_the_occupancy_grid_never_disagree() {
        let mut snake = Snake::new(Some(4), ColorMode::Bw);
        for _ in 0..1_500 {
            snake.step();
            if snake.phase != Phase::Playing {
                break;
            }

            let lit = snake.occupied.iter().filter(|cell| **cell).count();
            assert_eq!(lit, snake.body.len(), "occupancy drifted from the body");
            for cell in &snake.body {
                assert!(snake.occupied[slot(*cell).unwrap()], "body cell not marked");
            }
        }
    }

    #[test]
    fn the_snake_never_overlaps_itself() {
        let mut snake = Snake::new(Some(9), ColorMode::Bw);
        for _ in 0..1_500 {
            snake.step();
            if snake.phase != Phase::Playing {
                break;
            }
            let mut seen = [false; PIXELS];
            for cell in &snake.body {
                let index = slot(*cell).expect("body cell on the board");
                assert!(!seen[index], "the snake ate itself at {cell:?}");
                seen[index] = true;
            }
        }
    }

    #[test]
    fn the_robot_survives_long_enough_to_be_worth_watching() {
        // Naive food-chasing bots trap themselves within a few dozen moves;
        // this is the regression test for the tail-reachability check. Runs now
        // end in a win at MAX_LENGTH, so the bar is that the robot keeps winning
        // round after round and never once dies.
        for seed in [1u64, 2, 3, 5, 8] {
            let mut snake = Snake::new(Some(seed), ColorMode::Bw);
            let mut wins = 0;

            // A run to MAX_LENGTH takes around 580 moves, so this is a handful
            // of complete rounds rather than the couple it used to be.
            for step in 0..6_000 {
                match snake.phase {
                    Phase::Playing => snake.step(),
                    Phase::Won(_) => {
                        wins += 1;
                        snake.restart();
                    }
                    Phase::Dying(_) => {
                        panic!("seed {seed} died after {step} moves, on win {wins}")
                    }
                }
            }

            assert!(wins >= 5, "seed {seed} only completed {wins} runs");
        }
    }

    #[test]
    fn the_snake_stays_a_connected_line_that_never_teleports() {
        // Absence of duplicate cells is not enough to prove the snake never
        // crosses itself: a body whose consecutive segments stop touching draws
        // exactly like one that walked through its own side.
        let mut snake = Snake::new(Some(3), ColorMode::Bw);
        let mut previous_head = snake.head();
        let mut previous_length = snake.body.len();
        let mut checked = 0;

        for step in 0..4_000 {
            snake.update(Duration::from_millis(33));
            // A round ending resets the board, which is a legitimate jump.
            if snake.phase != Phase::Playing || snake.body.len() < previous_length {
                previous_head = snake.head();
                previous_length = snake.body.len();
                continue;
            }
            checked += 1;

            let head = snake.head();
            let travelled = (head.x - previous_head.x).abs() + (head.y - previous_head.y).abs();
            assert!(
                travelled <= 1,
                "the head jumped {travelled} cells at step {step}"
            );

            for pair in snake.body.iter().collect::<Vec<_>>().windows(2) {
                let gap = (pair[0].x - pair[1].x).abs() + (pair[0].y - pair[1].y).abs();
                assert_eq!(gap, 1, "the body broke apart at step {step}: {pair:?}");
            }

            let grown = snake.body.len().abs_diff(previous_length);
            assert!(grown <= 1, "the body changed by {grown} cells at once");

            previous_head = head;
            previous_length = snake.body.len();
        }

        // Without this the test would pass by dying on move three and checking
        // nothing at all.
        assert!(
            checked > 3_500,
            "only {checked} frames were actually checked"
        );
    }

    #[test]
    fn the_head_rarely_ends_up_against_its_own_body() {
        // Reported twice as "the snake passes through its body". It never does:
        // on a one-bit panel a head touching a non-consecutive segment merges
        // into it, so the head disappears into the blob and reappears further
        // on, which looks exactly like walking through. Ranking safe moves by
        // how much they hug is what keeps this down; the rest is corridors
        // where there is no alternative. It climbs with length — 1% below ten
        // cells, 16% in the thirties — so the bar has to leave room for the
        // long end of a run to MAX_LENGTH. This seed sits at 7%.
        let mut snake = Snake::new(Some(11), ColorMode::Bw);
        let mut steps = 0;
        let mut hugging = 0;

        for _ in 0..1_500 {
            // Keep going across rounds: the interesting moves are the ones made
            // by a long snake, and every round grows one.
            if snake.phase != Phase::Playing {
                snake.restart();
                continue;
            }
            let before = snake.head();
            snake.step();
            if snake.phase != Phase::Playing {
                continue;
            }
            let head = snake.head();
            if head == before {
                continue;
            }

            steps += 1;
            // The neck is excluded, so anything left is a segment it is now
            // running alongside.
            if snake.hugs_excluding_neck(head, before) > 0 {
                hugging += 1;
            }
        }

        assert!(steps > 1_000, "only {steps} moves were observed");
        let percent = 100 * hugging / steps;
        assert!(
            percent <= 12,
            "the head hugged its own body on {percent}% of moves"
        );
    }

    #[test]
    fn a_boxed_in_snake_dies_rather_than_walking_through_itself() {
        let mut snake = Snake::new(Some(1), ColorMode::Bw);
        // Wall the head in on all four sides.
        snake.occupied = [true; PIXELS];
        snake.step();
        assert!(matches!(snake.phase, Phase::Dying(_)));
    }

    #[test]
    fn death_restarts_the_game() {
        let mut snake = Snake::new(Some(1), ColorMode::Bw);
        snake.die();
        assert!(matches!(snake.phase, Phase::Dying(_)));

        for _ in 0..60 {
            snake.update(Duration::from_millis(33));
        }
        assert_eq!(snake.phase, Phase::Playing);
        assert_eq!(snake.body.len(), 3, "the board did not reset");
    }

    #[test]
    fn a_won_run_retracts_into_its_head_without_ever_blinking() {
        // Reported as "the snake kills itself once it gets big". It does not —
        // it reaches MAX_LENGTH and wins. The win used to flash the body at
        // 2 Hz where a death flashes at 5 Hz, a distinction nobody can make on
        // a one-bit panel, so every finished run read as a death.
        let mut snake = Snake::new(Some(7), ColorMode::Bw);
        while snake.phase == Phase::Playing {
            snake.step();
        }
        assert!(matches!(snake.phase, Phase::Won(_)), "the run should be won");

        let body: Vec<Cell> = snake.body.iter().copied().collect();
        let mut previous = body.len();
        let mut frames = 0;

        while matches!(snake.phase, Phase::Won(_)) {
            let mut canvas = Canvas::new();
            snake.render(&mut canvas, Area::FULL);

            let lit = body
                .iter()
                .filter(|cell| canvas.get(cell.x, cell.y) >= BW_THRESHOLD)
                .count();
            // Whatever is left has to be the head end of the snake: the tail
            // retracts into the head, it does not dissolve into holes.
            for (index, cell) in body.iter().enumerate() {
                assert_eq!(
                    canvas.get(cell.x, cell.y) >= BW_THRESHOLD,
                    index < lit,
                    "segment {index} out of order on frame {frames}"
                );
            }
            assert!(
                lit <= previous,
                "the body came back on frame {frames}: {previous} then {lit}"
            );

            previous = lit;
            frames += 1;
            snake.update(Duration::from_millis(33));
        }

        assert!(frames > 30, "only {frames} frames of win animation");
        assert!(previous <= 1, "{previous} segments still lit at the end");
        assert_eq!(snake.phase, Phase::Playing, "the next game did not start");
        assert_eq!(snake.body.len(), 3, "the board did not reset");
    }

    #[test]
    fn a_death_still_blinks_where_a_win_does_not() {
        // The other half of the pair above: the two endings have to look like
        // different things, so this one must keep toggling.
        let mut snake = Snake::new(Some(1), ColorMode::Bw);
        snake.die();
        let head = snake.head();
        let (mut lit, mut dark) = (0, 0);

        while matches!(snake.phase, Phase::Dying(_)) {
            let mut canvas = Canvas::new();
            snake.render(&mut canvas, Area::FULL);
            if canvas.get(head.x, head.y) >= BW_THRESHOLD {
                lit += 1;
            } else {
                dark += 1;
            }
            snake.update(Duration::from_millis(33));
        }

        assert!(lit > 3, "the death flash never lit up");
        assert!(dark > 3, "the death flash never went dark");
    }

    #[test]
    fn the_retract_runs_from_the_whole_body_down_to_nothing() {
        assert_eq!(retracted_length(40, WIN_RETRACT), 40);
        assert_eq!(retracted_length(40, WIN_RETRACT / 2.0), 20);
        assert_eq!(retracted_length(40, 0.0), 0);
        assert_eq!(retracted_length(40, -1.0), 0, "clamped past the end");
        assert_eq!(
            retracted_length(40, WIN_RETRACT * 2.0),
            40,
            "clamped before the start"
        );
    }

    #[test]
    fn the_head_is_the_brightest_pixel() {
        let mut snake = Snake::new(Some(2), ColorMode::Bw);
        for _ in 0..40 {
            snake.step();
        }

        let mut canvas = Canvas::new();
        snake.render(&mut canvas, Area::FULL);

        let head = snake.body.front().copied().unwrap();
        assert_eq!(canvas.get(head.x, head.y), 255);
        for cell in snake.body.iter().skip(1) {
            assert!(
                canvas.get(cell.x, cell.y) < 255,
                "body as bright as the head"
            );
            assert!(canvas.get(cell.x, cell.y) > 0, "body segment unlit");
        }
    }

    #[test]
    fn no_part_of_the_snake_ever_blinks_out() {
        // The bug this pins down was reported as "the snake seems to pass
        // through its own body". It never did: the head blinked at 7 Hz, which
        // hid it for longer than the 110 ms it takes to move, so it kept
        // reappearing a cell or two further on — and on a coiled snake that
        // looks exactly like walking through yourself.
        for mode in [ColorMode::Bw, ColorMode::Greyscale] {
            let mut snake = Snake::new(Some(2), mode);

            for frame in 0..400 {
                snake.update(Duration::from_millis(33));
                if snake.phase != Phase::Playing {
                    continue;
                }

                let mut canvas = Canvas::new();
                snake.render(&mut canvas, Area::FULL);
                for (index, cell) in snake.body.iter().enumerate() {
                    assert!(
                        canvas.get(cell.x, cell.y) >= BW_THRESHOLD,
                        "segment {index} went dark on frame {frame} in {mode}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_food_stays_lit_most_of_the_time() {
        // A food that is dark half the time is a strobe on a scene made of four
        // pixels. It should read as a twinkle.
        let mut snake = Snake::new(Some(2), ColorMode::Bw);
        let food = snake.food;
        let mut lit = 0;

        for _ in 0..300 {
            snake.elapsed += 0.01;
            let mut canvas = Canvas::new();
            snake.render(&mut canvas, Area::FULL);
            if canvas.get(food.x, food.y) >= BW_THRESHOLD {
                lit += 1;
            }
        }
        assert!((200..=280).contains(&lit), "food lit on {lit}/300 frames");
    }

    #[test]
    fn blinking_follows_its_duty_cycle() {
        assert!(super::blinking(0.0, 1.0, 0.75));
        assert!(
            super::blinking(0.70, 1.0, 0.75),
            "still inside the lit share"
        );
        assert!(!super::blinking(0.80, 1.0, 0.75), "should be dark");
        assert!(super::blinking(1.10, 1.0, 0.75), "next cycle, lit again");
    }

    #[test]
    fn body_brightness_fades_toward_the_tail() {
        assert!(body_level(1, 10) > body_level(9, 10));
        assert!(body_level(9, 10) >= 45);
        assert!(body_level(1, 10) <= 190);
    }

    #[test]
    fn direction_deltas_are_unit_steps() {
        let origin = Cell { x: 4, y: 4 };
        for direction in Direction::ALL {
            let moved = origin.shifted(direction);
            let distance = (moved.x - origin.x).abs() + (moved.y - origin.y).abs();
            assert_eq!(distance, 1);
        }
    }
}
