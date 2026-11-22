//! An agent's icon (2026-11-20, owner design): one of Melissa's own simple shapes in one of a
//! few colours, chosen by the user (like a chat app's bot picture). Melissa (the lead agent)
//! always has the honey hexagon, so neither honey nor the hexagon is offered here.
//!
//! Only names are stored (`{"shape": "cube", "color": "teal"}`); the web UI draws the shapes
//! and maps the colours to its tokens. Unknown names (an edited file) fall back to the
//! default for the agent's id.

use serde::{Deserialize, Serialize};

/// The shapes, in the order the picker shows them.
pub const SHAPES: &[&str] = &[
    "cube", "circle", "square", "triangle", "diamond", "bolt", "leaf", "drop", "cloud", "server",
    "shield", "clock",
];

/// The colours, in the order the picker shows them.
pub const COLORS: &[&str] = &[
    "green", "teal", "blue", "violet", "rose", "orange", "lime", "slate",
];

/// A shape in a colour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentIcon {
    pub shape: String,
    pub color: String,
}

impl AgentIcon {
    /// An icon from known names (None for anything else).
    pub fn new(shape: &str, color: &str) -> Option<Self> {
        let shape = SHAPES.iter().find(|s| **s == shape.trim())?;
        let color = COLORS.iter().find(|c| **c == color.trim())?;
        Some(Self {
            shape: (*shape).to_string(),
            color: (*color).to_string(),
        })
    }

    /// The icon an agent gets until the user picks one: from its id's number, so agents made
    /// one after another look different.
    pub fn default_for(id: &str) -> Self {
        let n = id
            .rsplit_once(".a")
            .and_then(|(_, n)| n.parse::<usize>().ok())
            .unwrap_or(0);
        Self {
            shape: SHAPES[n % SHAPES.len()].to_string(),
            color: COLORS[(n / SHAPES.len() + n) % COLORS.len()].to_string(),
        }
    }

    /// An app's icon until someone picks one (2026-11-22): derived from its name (FNV-1a, then
    /// MurmurHash3's finalizer, as FNV's low bits vary little across short names), so it is
    /// the same on every page, after a restart and after a reinstall, without being stored.
    /// Shape and colour come from different bits, so apps that share a shape mostly differ
    /// in colour.
    pub fn for_app(name: &str) -> Self {
        let mut h: u32 = 0x811c_9dc5;
        for b in name.bytes() {
            h ^= u32::from(b);
            h = h.wrapping_mul(0x0100_0193);
        }
        h ^= h >> 16;
        h = h.wrapping_mul(0x85eb_ca6b);
        h ^= h >> 13;
        h = h.wrapping_mul(0xc2b2_ae35);
        h ^= h >> 16;
        let shape = (h % SHAPES.len() as u32) as usize;
        let color = ((h >> 16) % COLORS.len() as u32) as usize;
        Self {
            shape: SHAPES[shape].to_string(),
            color: COLORS[color].to_string(),
        }
    }

    /// This icon if its names are known, else the default for `id`.
    pub fn or_default(self, id: &str) -> Self {
        Self::new(&self.shape, &self.color).unwrap_or_else(|| Self::default_for(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_known_names_and_defaults_differ() {
        assert!(AgentIcon::new("cube", "teal").is_some());
        assert!(AgentIcon::new("hexagon", "teal").is_none(), "Melissa's own");
        assert!(AgentIcon::new("cube", "honey").is_none());
        assert!(AgentIcon::new("<svg>", "teal").is_none());
        let a = AgentIcon::default_for("d-1234abcd.a1");
        let b = AgentIcon::default_for("d-1234abcd.a2");
        assert_ne!(a, b);
        assert!(AgentIcon::new(&a.shape, &a.color).is_some());
        let odd = AgentIcon {
            shape: "x".into(),
            color: "y".into(),
        };
        assert_eq!(odd.or_default("d-1234abcd.a2"), b);
    }

    #[test]
    fn app_icons_are_stable_known_and_varied() {
        let a = AgentIcon::for_app("minecraft");
        assert_eq!(a, AgentIcon::for_app("minecraft"));
        assert!(AgentIcon::new(&a.shape, &a.color).is_some());
        let names = [
            "minecraft",
            "jellyfin",
            "navidrome",
            "forgejo",
            "syncthing",
            "luanti",
        ];
        let icons: std::collections::BTreeSet<_> = names
            .iter()
            .map(|n| {
                let i = AgentIcon::for_app(n);
                (i.shape, i.color)
            })
            .collect();
        assert!(icons.len() >= 5, "{icons:?}");
    }
}
