//! What the panels were showing last time, so a restart picks up where it left.
//!
//! Three keys in a `key=value` file. A serialiser would be a dependency and a
//! format to version, for something a person should be able to read and fix
//! with an editor.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::{debug, warn};

use crate::control::PanelName;
use crate::scene::SceneKind;

/// What survives a restart.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct State {
    /// Scene on the left module, if one was ever set.
    pub left: Option<SceneKind>,
    /// Scene on the right module, if one was ever set.
    pub right: Option<SceneKind>,
    /// Panel brightness, if it was ever set.
    pub brightness: Option<u8>,
}

impl State {
    /// The scene remembered for `panel`.
    #[must_use]
    pub fn scene(&self, panel: PanelName) -> Option<SceneKind> {
        match panel {
            PanelName::Left => self.left,
            PanelName::Right => self.right,
        }
    }

    /// Remembers a scene for `panel`.
    pub fn set_scene(&mut self, panel: PanelName, scene: SceneKind) {
        match panel {
            PanelName::Left => self.left = Some(scene),
            PanelName::Right => self.right = Some(scene),
        }
    }
}

/// Renders the state as the file's contents.
#[must_use]
pub fn format(state: &State) -> String {
    let mut out = String::from("# Written by ledmat; edit freely, it is read back as is.\n");
    if let Some(scene) = state.left {
        let _ = writeln!(out, "left={scene}");
    }
    if let Some(scene) = state.right {
        let _ = writeln!(out, "right={scene}");
    }
    if let Some(brightness) = state.brightness {
        let _ = writeln!(out, "brightness={brightness}");
    }
    out
}

/// Reads the state back, ignoring anything it does not understand.
///
/// A stale key from an older version, or a typo made by hand, should cost the
/// one setting it names — not the whole file.
#[must_use]
pub fn parse(text: &str) -> State {
    let mut pairs = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            pairs.insert(key.trim().to_owned(), value.trim().to_owned());
        }
    }

    let scene = |key: &str| -> Option<SceneKind> {
        let raw = pairs.get(key)?;
        match crate::control::scene_from_str(raw) {
            Ok(kind) => Some(kind),
            Err(_) => {
                warn!(key, value = raw, "ignoring an unknown scene in the saved state");
                None
            }
        }
    };

    State {
        left: scene("left"),
        right: scene("right"),
        brightness: pairs.get("brightness").and_then(|raw| raw.parse().ok()),
    }
}

/// Where the state lives, following the XDG base directory layout.
#[must_use]
pub fn default_path() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME").map_or_else(
        || {
            std::env::var_os("HOME").map_or_else(
                || PathBuf::from("."),
                |home| PathBuf::from(home).join(".local/state"),
            )
        },
        PathBuf::from,
    );
    base.join("ledmat/state")
}

/// Loads the state, treating a missing or unreadable file as "nothing saved".
#[must_use]
pub fn load(path: &Path) -> State {
    match fs::read_to_string(path) {
        Ok(text) => {
            let state = parse(&text);
            debug!(path = %path.display(), ?state, "restored the saved state");
            state
        }
        Err(error) => {
            debug!(path = %path.display(), ?error, "no saved state");
            State::default()
        }
    }
}

/// Writes the state, replacing the file in one step.
///
/// # Errors
///
/// Fails if the directory cannot be created or the file cannot be written.
pub fn save(path: &Path, state: &State) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    // Write beside the target and rename: a crash halfway through then leaves
    // the previous state intact rather than a truncated file.
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, format(state))
        .with_context(|| format!("writing {}", temporary.display()))?;
    fs::rename(&temporary, path).with_context(|| format!("replacing {}", path.display()))?;
    debug!(path = %path.display(), "saved the state");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{State, format, load, parse, save};
    use crate::control::PanelName;
    use crate::scene::SceneKind;

    fn full() -> State {
        State {
            left: Some(SceneKind::Clock),
            right: Some(SceneKind::Snake),
            brightness: Some(42),
        }
    }

    #[test]
    fn a_state_survives_a_round_trip() {
        assert_eq!(parse(&format(&full())), full());
    }

    #[test]
    fn an_empty_state_writes_only_a_comment_and_reads_back_empty() {
        let text = format(&State::default());
        assert!(text.starts_with('#'));
        assert_eq!(parse(&text), State::default());
    }

    #[test]
    fn an_unknown_scene_costs_only_its_own_key() {
        // A state file written by a newer version, or edited by hand, must not
        // take the rest of the settings down with it.
        let state = parse("left=tetris\nright=snake\nbrightness=30\n");
        assert_eq!(state.left, None);
        assert_eq!(state.right, Some(SceneKind::Snake));
        assert_eq!(state.brightness, Some(30));
    }

    #[test]
    fn junk_is_ignored_rather_than_fatal() {
        let state = parse("\n\n# a comment\nnonsense\nbrightness=oops\nleft=pong\n");
        assert_eq!(state.left, Some(SceneKind::Pong));
        assert_eq!(state.brightness, None);
    }

    #[test]
    fn whitespace_around_keys_and_values_is_tolerated() {
        let state = parse("  left =  gauges  \n\tbrightness\t=\t7\n");
        assert_eq!(state.left, Some(SceneKind::Gauges));
        assert_eq!(state.brightness, Some(7));
    }

    #[test]
    fn scenes_are_remembered_per_panel() {
        let mut state = State::default();
        state.set_scene(PanelName::Left, SceneKind::Battery);
        assert_eq!(state.scene(PanelName::Left), Some(SceneKind::Battery));
        assert_eq!(state.scene(PanelName::Right), None);
    }

    #[test]
    fn saving_then_loading_returns_the_same_state() {
        let directory = std::env::temp_dir().join(format!("ledmat-state-{}", std::process::id()));
        let path = directory.join("nested/state");

        save(&path, &full()).expect("save");
        assert_eq!(load(&path), full(), "the file did not come back");

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn loading_a_missing_file_is_not_an_error() {
        assert_eq!(load(std::path::Path::new("/nonexistent/ledmat")), State::default());
    }
}
