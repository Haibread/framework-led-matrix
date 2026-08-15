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
use crate::scene::{Scene, rng_from};

/// Time between two moves. Slow enough to follow with your eyes.
const STEP_INTERVAL: f32 = 0.11;
/// How long the death animation lasts before a new game starts.
const DEATH_FLASH: f32 = 1.1;
/// Longest simulated step, so a stalled thread does not fast-forward the game.
const MAX_STEP: f32 = 0.1;

/// Moves allowed without eating before the run is declared stuck.
const STARVATION_LIMIT: u32 = 900;

const HEAD_LEVEL: u8 = 255;
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
}

/// A self-playing game of Snake.
pub struct Snake {
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
    pub fn new(seed: Option<u64>) -> Self {
        let mut snake = Self {
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
            self.phase = Phase::Dying(DEATH_FLASH);
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

    /// The first step of a path to the food that does not trap the snake.
    fn safe_path_to_food(&self) -> Option<Direction> {
        let mut blocked = self.occupied;
        // By the time the snake gets there, the tail will have moved on.
        if let Some(index) = self.body.back().copied().and_then(slot) {
            blocked[index] = false;
        }

        let path = search(self.head(), self.food, &blocked)?;
        if !self.survives(&path) {
            return None;
        }
        direction_between(self.head(), *path.first()?)
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
            Phase::Playing => {
                self.step_timer += dt;
                while self.step_timer >= STEP_INTERVAL && self.phase == Phase::Playing {
                    self.step_timer -= STEP_INTERVAL;
                    self.step();
                }
            }
        }
    }

    fn render(&self, canvas: &mut Canvas) {
        if let Phase::Dying(remaining) = self.phase {
            let lit = canvas::floor_pixel(remaining * 9.0) % 2 == 0;
            let level = if lit { 210 } else { 30 };
            for cell in &self.body {
                canvas.set_max(cell.x, cell.y, level);
            }
            return;
        }

        // A slow pulse so the food reads differently from the body.
        let pulse = 0.45f32.mul_add((self.elapsed * 6.0).sin(), 0.55);
        canvas.set_max(self.food.x, self.food.y, canvas::level(pulse));

        let length = self.body.len();
        for (index, cell) in self.body.iter().enumerate() {
            let level = if index == 0 {
                HEAD_LEVEL
            } else {
                body_level(index, length)
            };
            canvas.set_max(cell.x, cell.y, level);
        }
    }
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
    use super::{Cell, Direction, PIXELS, Phase, Snake, body_level, search, slot};
    use crate::canvas::{self, Canvas};
    use crate::scene::Scene;
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
        let snake = Snake::new(Some(1));
        assert_eq!(snake.phase, Phase::Playing);
        assert_eq!(snake.body.len(), 3);
        assert!(!snake.body.contains(&snake.food));
    }

    #[test]
    fn the_body_and_the_occupancy_grid_never_disagree() {
        let mut snake = Snake::new(Some(4));
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
        let mut snake = Snake::new(Some(9));
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
        // this is the regression test for the tail-reachability check.
        for seed in [1u64, 2, 3, 5, 8] {
            let mut snake = Snake::new(Some(seed));
            for step in 0..1_200 {
                snake.step();
                assert_eq!(
                    snake.phase,
                    Phase::Playing,
                    "seed {seed} died after {step} moves"
                );
            }
            assert!(
                snake.body.len() > 30,
                "seed {seed} only grew to {}",
                snake.body.len()
            );
        }
    }

    #[test]
    fn a_boxed_in_snake_dies_rather_than_walking_through_itself() {
        let mut snake = Snake::new(Some(1));
        // Wall the head in on all four sides.
        snake.occupied = [true; PIXELS];
        snake.step();
        assert!(matches!(snake.phase, Phase::Dying(_)));
    }

    #[test]
    fn death_restarts_the_game() {
        let mut snake = Snake::new(Some(1));
        snake.die();
        assert!(matches!(snake.phase, Phase::Dying(_)));

        for _ in 0..60 {
            snake.update(Duration::from_millis(33));
        }
        assert_eq!(snake.phase, Phase::Playing);
        assert_eq!(snake.body.len(), 3, "the board did not reset");
    }

    #[test]
    fn the_head_is_the_brightest_pixel() {
        let mut snake = Snake::new(Some(2));
        for _ in 0..40 {
            snake.step();
        }

        let mut canvas = Canvas::new();
        snake.render(&mut canvas);

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
