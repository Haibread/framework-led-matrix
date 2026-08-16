//! Pong, played by two robots that are deliberately imperfect.
//!
//! A pong AI that simply tracks the predicted impact point never misses, and a
//! rally that never ends is not worth watching. Both paddles here have a
//! capped speed, a reaction delay before they start tracking, and a fresh aim
//! error on every exchange — so they trade points like players, not like maths.

use std::time::Duration;

use rand::RngExt;
use rand::rngs::StdRng;

use crate::canvas::{self, Canvas};
use crate::device::ColorMode;
use crate::font;
use crate::scene::{Area, Scene, rng_from};

// The field is the panel, in pixels, as floats.
const MAX_X: f32 = 8.0;
const CENTRE_X: f32 = 4.0;
const CENTRE_Y: f32 = 17.0;
const _: () = assert!(canvas::WIDTH == 9 && canvas::HEIGHT == 34);

/// Rows the paddles are drawn on.
const TOP_ROW: i32 = 0;
const BOTTOM_ROW: i32 = 33;
/// Rows the ball bounces off, one pixel inside the paddles.
const TOP_PLANE: f32 = 1.0;
const BOTTOM_PLANE: f32 = 32.0;

/// Paddles are three pixels wide, so one pixel either side of their centre.
const PADDLE_HALF: f32 = 1.0;
const PADDLE_SPEED: f32 = 13.0;

const BALL_START_SPEED: f32 = 16.0;
const BALL_MAX_SPEED: f32 = 34.0;
/// Speed multiplier applied on every successful return.
const SPEED_GAIN: f32 = 1.045;
/// How much hitting the edge of a paddle deflects the ball sideways.
const SPIN: f32 = 5.0;
/// A rally stalls if the ball goes too flat; keep this share of the speed vertical.
const MIN_VERTICAL_SHARE: f32 = 0.45;

const REACTION_MIN: f32 = 0.12;
const REACTION_MAX: f32 = 0.42;
/// Peak aiming error, in pixels. Slightly wider than the paddle's half width,
/// so a bad read genuinely misses.
const AIM_ERROR: f32 = 1.35;

const SERVE_DELAY: f32 = 0.7;
const SCORE_DELAY: f32 = 1.5;
/// Score to reach before the match resets.
const WIN_SCORE: u32 = 9;
/// Longest simulated step, so a stalled thread does not teleport the ball.
const MAX_STEP: f32 = 0.1;

/// Brightness of the dotted half-way line.
const MIDLINE_LEVEL: u8 = 18;

/// The ball, in continuous field coordinates.
#[derive(Clone, Copy, Debug)]
struct Ball {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
}

/// One robot paddle.
#[derive(Clone, Copy, Debug)]
struct Paddle {
    /// Centre of the paddle, in pixels.
    x: f32,
    /// Aim error for the current exchange, in pixels.
    aim_error: f32,
    /// Time left before this paddle starts tracking the ball.
    reaction: f32,
}

/// What the match is doing right now.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    /// Waiting to serve; the ball is parked in the middle.
    Serve(f32),
    /// The ball is in play.
    Rally,
    /// Showing the score after a point.
    Scored(f32),
}

/// A self-playing game of Pong.
pub struct Pong {
    mode: ColorMode,
    rng: StdRng,
    ball: Ball,
    top: Paddle,
    bottom: Paddle,
    score_top: u32,
    score_bottom: u32,
    phase: Phase,
}

impl Pong {
    /// Starts a match. `seed` makes the run reproducible.
    #[must_use]
    pub fn new(seed: Option<u64>, mode: ColorMode) -> Self {
        Self {
            mode,
            rng: rng_from(seed),
            ball: Ball {
                x: CENTRE_X,
                y: CENTRE_Y,
                vx: 0.0,
                vy: 0.0,
            },
            top: Paddle {
                x: CENTRE_X,
                aim_error: 0.0,
                reaction: 0.0,
            },
            bottom: Paddle {
                x: CENTRE_X,
                aim_error: 0.0,
                reaction: 0.0,
            },
            score_top: 0,
            score_bottom: 0,
            phase: Phase::Serve(SERVE_DELAY),
        }
    }

    /// Parks the ball in the middle, ready for the next serve.
    fn park_ball(&mut self) {
        self.ball = Ball {
            x: CENTRE_X,
            y: CENTRE_Y,
            vx: 0.0,
            vy: 0.0,
        };
    }

    /// Sends the ball off at a random shallow angle.
    fn launch(&mut self) {
        let angle: f32 = self.rng.random_range(-0.55..0.55);
        let direction = if self.rng.random_bool(0.5) { 1.0 } else { -1.0 };
        self.ball.vx = angle.sin() * BALL_START_SPEED;
        self.ball.vy = angle.cos() * BALL_START_SPEED * direction;
        self.arm_receiver();
    }

    /// Gives the paddle the ball is heading for a fresh reaction and aim error.
    fn arm_receiver(&mut self) {
        let reaction = self.rng.random_range(REACTION_MIN..REACTION_MAX);
        let aim_error = self.rng.random_range(-AIM_ERROR..AIM_ERROR);
        let paddle = if self.ball.vy > 0.0 {
            &mut self.bottom
        } else {
            &mut self.top
        };
        paddle.reaction = reaction;
        paddle.aim_error = aim_error;
    }

    /// Moves the ball, bouncing off the side walls and the paddles.
    fn advance_ball(&mut self, dt: f32) {
        self.ball.x += self.ball.vx * dt;
        self.ball.y += self.ball.vy * dt;

        if self.ball.x < 0.0 {
            self.ball.x = -self.ball.x;
            self.ball.vx = -self.ball.vx;
        } else if self.ball.x > MAX_X {
            self.ball.x = 2.0f32.mul_add(MAX_X, -self.ball.x);
            self.ball.vx = -self.ball.vx;
        }

        if self.ball.vy < 0.0 && self.ball.y <= TOP_PLANE {
            if covers(self.top.x, self.ball.x) {
                self.bounce(TOP_PLANE, self.top.x);
            } else {
                self.score(true);
            }
        } else if self.ball.vy > 0.0 && self.ball.y >= BOTTOM_PLANE {
            if covers(self.bottom.x, self.ball.x) {
                self.bounce(BOTTOM_PLANE, self.bottom.x);
            } else {
                self.score(false);
            }
        }
    }

    /// Reflects the ball off a paddle, adding spin and a little speed.
    fn bounce(&mut self, plane: f32, paddle_x: f32) {
        self.ball.y = 2.0f32.mul_add(plane, -self.ball.y);
        self.ball.vy = -self.ball.vy;

        let offset = ((self.ball.x - paddle_x) / (PADDLE_HALF + 0.5)).clamp(-1.0, 1.0);
        self.ball.vx = offset.mul_add(SPIN, self.ball.vx);

        let current = self.ball.vx.hypot(self.ball.vy);
        let wanted = (current * SPEED_GAIN).min(BALL_MAX_SPEED);
        if current > f32::EPSILON {
            let scale = wanted / current;
            self.ball.vx *= scale;
            self.ball.vy *= scale;
        }

        // Keep enough vertical speed that the rally actually progresses.
        let min_vertical = wanted * MIN_VERTICAL_SHARE;
        if self.ball.vy.abs() < min_vertical {
            self.ball.vy = min_vertical * self.ball.vy.signum();
            let horizontal = self
                .ball
                .vy
                .mul_add(-self.ball.vy, wanted * wanted)
                .max(0.0)
                .sqrt();
            self.ball.vx = horizontal * self.ball.vx.signum();
        }

        self.arm_receiver();
    }

    /// Awards a point and freezes the match to show the score.
    fn score(&mut self, bottom_scored: bool) {
        if bottom_scored {
            self.score_bottom += 1;
        } else {
            self.score_top += 1;
        }
        self.phase = Phase::Scored(SCORE_DELAY);
    }

    /// Runs both paddles for one step.
    fn drive_paddles(&mut self, dt: f32) {
        let ball = self.ball;
        drive(&mut self.top, &ball, TOP_PLANE, ball.vy < 0.0, dt);
        drive(&mut self.bottom, &ball, BOTTOM_PLANE, ball.vy > 0.0, dt);
    }

    /// Walks both paddles back toward the middle between points.
    fn recentre_paddles(&mut self, dt: f32) {
        let step = PADDLE_SPEED * dt;
        for paddle in [&mut self.top, &mut self.bottom] {
            let delta = (CENTRE_X - paddle.x).clamp(-step, step);
            paddle.x += delta;
        }
    }

    /// Draws a paddle centred on `x`.
    fn draw_paddle(canvas: &mut Canvas, x: f32, row: i32) {
        let centre = canvas::to_pixel(x);
        canvas.hline(centre - 1, centre + 1, row, u8::MAX);
    }

    /// Draws the ball.
    ///
    /// In greyscale it is spread over the pixels it overlaps: the panel has
    /// 8-bit brightness per LED, so a ball between two pixels lights both
    /// proportionally and reads as smooth motion rather than a dot snapping
    /// from cell to cell.
    ///
    /// In black and white that same spread is a liability — thresholding turns
    /// it into a ball that flickers between one and two pixels wide as it
    /// travels — so it snaps to the nearest pixel and stays crisp.
    fn draw_ball(&self, canvas: &mut Canvas) {
        if self.mode == ColorMode::Bw {
            canvas.set_max(
                canvas::to_pixel(self.ball.x),
                canvas::to_pixel(self.ball.y),
                u8::MAX,
            );
            return;
        }

        let x0 = canvas::floor_pixel(self.ball.x);
        let y0 = canvas::floor_pixel(self.ball.y);
        let fx = self.ball.x - self.ball.x.floor();
        let fy = self.ball.y - self.ball.y.floor();

        for (dx, dy, weight) in [
            (0, 0, (1.0 - fx) * (1.0 - fy)),
            (1, 0, fx * (1.0 - fy)),
            (0, 1, (1.0 - fx) * fy),
            (1, 1, fx * fy),
        ] {
            canvas.set_max(x0 + dx, y0 + dy, canvas::level(weight));
        }
    }

    /// Draws both scores, one per half of the panel.
    fn draw_scores(&self, canvas: &mut Canvas) {
        let x = (canvas::WIDTH - font::GLYPH_WIDTH) / 2;
        font::draw_digit(canvas, self.score_top, x, 9, u8::MAX);
        font::draw_digit(canvas, self.score_bottom, x, 20, u8::MAX);
    }
}

impl Scene for Pong {
    fn name(&self) -> &'static str {
        "pong"
    }

    /// The whole panel. A game played on a strip is not the same game, so this
    /// asks for everything rather than degrading.
    fn min_height(&self) -> i32 {
        canvas::HEIGHT
    }

    fn update(&mut self, delta: Duration) {
        let dt = delta.as_secs_f32().min(MAX_STEP);

        match self.phase {
            Phase::Scored(remaining) => {
                let remaining = remaining - dt;
                if remaining <= 0.0 {
                    if self.score_top >= WIN_SCORE || self.score_bottom >= WIN_SCORE {
                        self.score_top = 0;
                        self.score_bottom = 0;
                    }
                    self.park_ball();
                    self.phase = Phase::Serve(SERVE_DELAY);
                } else {
                    self.phase = Phase::Scored(remaining);
                }
            }
            Phase::Serve(remaining) => {
                let remaining = remaining - dt;
                self.recentre_paddles(dt);
                if remaining <= 0.0 {
                    self.launch();
                    self.phase = Phase::Rally;
                } else {
                    self.phase = Phase::Serve(remaining);
                }
            }
            Phase::Rally => {
                self.advance_ball(dt);
                self.drive_paddles(dt);
            }
        }
    }

    fn render(&self, canvas: &mut Canvas, _area: Area) {
        if matches!(self.phase, Phase::Scored(_)) {
            self.draw_scores(canvas);
            return;
        }

        for x in (0..canvas::WIDTH).step_by(2) {
            canvas.set_max(x, 16, MIDLINE_LEVEL);
        }

        Self::draw_paddle(canvas, self.top.x, TOP_ROW);
        Self::draw_paddle(canvas, self.bottom.x, BOTTOM_ROW);
        self.draw_ball(canvas);
    }
}

/// Whether a paddle centred on `paddle_x` covers `ball_x`.
fn covers(paddle_x: f32, ball_x: f32) -> bool {
    (ball_x - paddle_x).abs() <= PADDLE_HALF + 0.5
}

/// Moves one paddle toward where it thinks it should be.
fn drive(paddle: &mut Paddle, ball: &Ball, plane: f32, incoming: bool, dt: f32) {
    let target = if incoming {
        paddle.reaction = (paddle.reaction - dt).max(0.0);
        if paddle.reaction > 0.0 {
            // Still reacting: hold position rather than cheat.
            paddle.x
        } else {
            predict_x(ball, plane) + paddle.aim_error
        }
    } else {
        CENTRE_X
    };

    let step = PADDLE_SPEED * dt;
    let delta = (target - paddle.x).clamp(-step, step);
    paddle.x = (paddle.x + delta).clamp(PADDLE_HALF, MAX_X - PADDLE_HALF);
}

/// Predicts where the ball crosses `target_y`, accounting for wall bounces.
fn predict_x(ball: &Ball, target_y: f32) -> f32 {
    if ball.vy.abs() < f32::EPSILON {
        return ball.x;
    }
    let time = (target_y - ball.y) / ball.vy;
    if time <= 0.0 {
        return ball.x;
    }
    fold(ball.vx.mul_add(time, ball.x))
}

/// Folds an unbounded x back into the field, mirroring at each wall.
fn fold(x: f32) -> f32 {
    let span = 2.0 * MAX_X;
    let wrapped = x.rem_euclid(span);
    if wrapped > MAX_X {
        span - wrapped
    } else {
        wrapped
    }
}

#[cfg(test)]
mod tests {
    use super::{Ball, MAX_X, Phase, Pong, WIN_SCORE, covers, fold, predict_x};
    use crate::canvas::Canvas;
    use crate::device::ColorMode;
    use crate::scene::{Area, Scene};
    use std::time::Duration;

    /// A frame at 30 fps.
    const FRAME: Duration = Duration::from_millis(33);

    fn run(pong: &mut Pong, frames: usize) {
        for _ in 0..frames {
            pong.update(FRAME);
        }
    }

    #[test]
    fn folding_mirrors_at_both_walls() {
        assert!((fold(3.0) - 3.0).abs() < 1e-5);
        assert!((fold(9.0) - 7.0).abs() < 1e-5, "past the right wall");
        assert!((fold(-1.0) - 1.0).abs() < 1e-5, "past the left wall");
        // Two full traversals land back where they started.
        assert!((fold(3.0 + 4.0 * MAX_X) - 3.0).abs() < 1e-4);
    }

    #[test]
    fn prediction_follows_a_straight_shot() {
        let ball = Ball {
            x: 2.0,
            y: 10.0,
            vx: 1.0,
            vy: 10.0,
        };
        // 2.2 seconds to travel 22 rows, drifting 2.2 columns right.
        assert!((predict_x(&ball, 32.0) - 4.2).abs() < 1e-4);
    }

    #[test]
    fn prediction_bounces_off_the_side_wall() {
        let ball = Ball {
            x: 7.0,
            y: 10.0,
            vx: 10.0,
            vy: 10.0,
        };
        // Unbounded it would reach x = 29: one pixel to the right wall, then
        // two full crossings, then five more back down from the right wall.
        assert!((predict_x(&ball, 32.0) - 3.0).abs() < 1e-4);
    }

    #[test]
    fn prediction_holds_still_when_the_ball_moves_away() {
        let ball = Ball {
            x: 6.0,
            y: 10.0,
            vx: 4.0,
            vy: -10.0,
        };
        assert!((predict_x(&ball, 32.0) - 6.0).abs() < 1e-5);
    }

    #[test]
    fn a_paddle_covers_its_three_pixels() {
        assert!(covers(4.0, 4.0));
        assert!(covers(4.0, 5.4));
        assert!(!covers(4.0, 5.6));
        assert!(!covers(4.0, 2.4));
    }

    #[test]
    fn the_ball_never_leaves_the_field() {
        let mut pong = Pong::new(Some(42), ColorMode::Bw);
        for _ in 0..6_000 {
            pong.update(FRAME);
            assert!(
                pong.ball.x >= -0.001 && pong.ball.x <= MAX_X + 0.001,
                "ball escaped sideways at x = {}",
                pong.ball.x
            );
            assert!(
                pong.ball.y >= -1.0 && pong.ball.y <= 35.0,
                "ball escaped vertically at y = {}",
                pong.ball.y
            );
        }
    }

    #[test]
    fn paddles_stay_on_the_panel() {
        let mut pong = Pong::new(Some(7), ColorMode::Bw);
        for _ in 0..6_000 {
            pong.update(FRAME);
            for paddle in [pong.top, pong.bottom] {
                assert!(
                    paddle.x >= 1.0 && paddle.x <= MAX_X - 1.0,
                    "paddle drifted off the panel at x = {}",
                    paddle.x
                );
            }
        }
    }

    #[test]
    fn both_robots_are_beatable_and_eventually_score() {
        let mut pong = Pong::new(Some(3), ColorMode::Bw);
        // Two minutes of play at 30 fps.
        run(&mut pong, 3_600);
        assert!(
            pong.score_top + pong.score_bottom > 0,
            "nobody scored in two minutes: the AI is too good to watch"
        );
    }

    #[test]
    fn a_match_resets_instead_of_running_past_the_win_score() {
        let mut pong = Pong::new(Some(11), ColorMode::Bw);
        for _ in 0..40_000 {
            pong.update(FRAME);
            assert!(pong.score_top <= WIN_SCORE && pong.score_bottom <= WIN_SCORE);
        }
    }

    #[test]
    fn rallies_speed_up_but_stay_bounded() {
        let mut pong = Pong::new(Some(5), ColorMode::Bw);
        for _ in 0..20_000 {
            pong.update(FRAME);
            let speed = pong.ball.vx.hypot(pong.ball.vy);
            assert!(
                speed <= super::BALL_MAX_SPEED + 0.001,
                "ball reached {speed} px/s"
            );
        }
    }

    #[test]
    fn the_score_screen_replaces_the_field() {
        let mut pong = Pong::new(Some(1), ColorMode::Bw);
        pong.score_top = 3;
        pong.score_bottom = 5;
        pong.phase = Phase::Scored(1.0);

        let mut canvas = Canvas::new();
        pong.render(&mut canvas, Area::FULL);

        assert_eq!(canvas.get(4, 33), 0, "the bottom paddle is still drawn");
        assert_ne!(canvas, Canvas::new(), "the score was not drawn");
    }

    #[test]
    fn the_ball_is_a_single_pixel_in_black_and_white() {
        // Thresholding an antialiased ball makes it flicker between one and two
        // pixels wide as it travels, which reads as a wobble rather than motion.
        let mut pong = Pong::new(Some(1), ColorMode::Bw);
        pong.phase = Phase::Rally;
        // Park it exactly between two pixels, the worst case for a splat.
        pong.ball = Ball {
            x: 3.5,
            y: 17.5,
            vx: 0.0,
            vy: 1.0,
        };

        let mut canvas = Canvas::new();
        pong.render(&mut canvas, Area::FULL);

        // Brighter than the midline, which also lives between the paddles.
        let lit = (0..9)
            .flat_map(|x| (2..32).map(move |y| (x, y)))
            .filter(|(x, y)| canvas.get(*x, *y) > super::MIDLINE_LEVEL)
            .count();
        assert_eq!(lit, 1, "the ball covered {lit} pixels");
    }

    #[test]
    fn the_ball_is_spread_over_two_pixels_in_greyscale() {
        let mut pong = Pong::new(Some(1), ColorMode::Greyscale);
        pong.phase = Phase::Rally;
        pong.ball = Ball {
            x: 3.5,
            y: 17.0,
            vx: 0.0,
            vy: 1.0,
        };

        let mut canvas = Canvas::new();
        pong.render(&mut canvas, Area::FULL);

        assert_eq!(canvas.get(3, 17), canvas.get(4, 17), "not evenly split");
        assert!(canvas.get(3, 17) > 0, "the ball vanished");
        assert!(canvas.get(3, 17) < 255, "the split was not partial");
    }

    #[test]
    fn paddles_are_drawn_on_the_outer_rows() {
        let mut pong = Pong::new(Some(1), ColorMode::Bw);
        run(&mut pong, 60);

        let mut canvas = Canvas::new();
        pong.render(&mut canvas, Area::FULL);

        let top_lit = (0..9).filter(|x| canvas.get(*x, 0) > 0).count();
        let bottom_lit = (0..9).filter(|x| canvas.get(*x, 33) > 0).count();
        assert_eq!(top_lit, 3);
        assert_eq!(bottom_lit, 3);
    }
}
