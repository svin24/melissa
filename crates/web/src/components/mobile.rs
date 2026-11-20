//! Phone views (2026-11-20, owner design "Melissa — Mobile"): at phone width (760px and
//! below) the agent list is a screen of its own with a tab bar at the bottom, a chat has a
//! header with a back button to that list, the agent details open as a bottom sheet and an
//! agent's "⋯" menu opens as an action sheet.
//!
//! These are the phone-only pieces; the screens reuse the desktop components (the sidebar is
//! the Agents screen and its `section_bar` the tab bar, `details_panel` the sheet,
//! `agent_menu` the action sheet). Everything
//! here is hidden above 760px by CSS, and works without script: the list and the chat are
//! separate pages, the sheet is a section after the chat, and the action sheet is a native
//! popover.
//!
//! Part of the fixed component library (`components.rs`); a separate file to keep it together.

use maud::{html, Markup};

use super::{
    agent::{avatar, Avatar},
    panel::{glyph, Glyph},
};

/// Which screen a page is on a phone. Above 760px both show the same page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhoneView {
    /// The agent list (home, `/`).
    List,
    /// A chat or another page (the default; no marker).
    Page,
}

/// The marker CSS reads to show the agent list instead of the page on phones (in `main`).
pub fn phone_view(v: PhoneView) -> Markup {
    html! {
        @if v == PhoneView::List {
            span hidden data-phone-view="list" {}
        }
    }
}

/// The back chevron (16-unit viewBox, as the other glyphs).
fn back_glyph() -> Markup {
    html! {
        svg.glyph viewBox="0 0 16 16" aria-hidden="true" focusable="false" {
            path d="M10 3 5 8l5 5" {}
        }
    }
}

/// The chat header's back button on phones: to the agent list.
pub fn back_link(href: &str, label: &str) -> Markup {
    html! {
        a.icon-button.back-link href=(href) title=(label) {
            (back_glyph())
            span.visually-hidden { (label) }
        }
    }
}

/// The round "+" next to the brand on the phone's agent list (the search row's "+" on wide
/// screens).
pub fn new_agent_round(href: &str) -> Markup {
    html! {
        a.icon-button.round-button.phone-new href=(href) title="New helper" {
            (glyph(Glyph::Plus))
            span.visually-hidden { "New helper" }
        }
    }
}

/// The grab handle at the top of a bottom sheet (drag it down to close, with script).
pub fn sheet_handle() -> Markup {
    html! {
        span.sheet-handle aria-hidden="true" data-sheet-handle {}
    }
}

/// Who a sheet is about: the icon and a short line under the title ("Minecraft agent ·
/// melissa"). Shown in the phone's sheets only.
pub struct SheetWho<'a> {
    pub avatar: Avatar<'a>,
    pub sub: &'a str,
}

/// The agent's icon of a sheet's header (phones only).
pub fn sheet_avatar(who: &SheetWho<'_>) -> Markup {
    html! { span.sheet-avatar { (avatar(&who.avatar, "av")) } }
}

/// The line under a sheet's title (phones only).
pub fn sheet_sub(who: &SheetWho<'_>) -> Markup {
    html! { p.sheet-sub { (who.sub) } }
}

/// Who an action sheet is about: the agent's icon, the device it runs on and what it needs or
/// does now ("Needs permission"), if anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuWho<'a> {
    pub avatar: Avatar<'a>,
    pub device: &'a str,
    pub status: Option<&'a str>,
}

/// The header of an action sheet: the agent's icon, its name and "device · status". The
/// wide menu leaves it out (CSS).
pub fn menu_head(who: &MenuWho<'_>, name: &str) -> Markup {
    html! {
        div.menu-head {
            (avatar(&who.avatar, "av"))
            div.menu-head-text {
                p.menu-head-name { (name) }
                p.menu-head-sub {
                    (who.device)
                    @if let Some(s) = who.status { " · " (s) }
                }
            }
        }
    }
}

/// The action sheet's Cancel button: closes the popover `id` (no script needed). Phones
/// only.
pub fn menu_cancel(id: &str) -> Markup {
    html! {
        button.menu-cancel type="button" popovertarget=(id) popovertargetaction="hide" { "Cancel" }
    }
}

/// The line under the composer on phones: changes on the device need the user's OK.
pub fn composer_caption(text: &str) -> Markup {
    html! { p.composer-caption { (text) } }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_list_view_leaves_a_marker() {
        assert_eq!(
            phone_view(PhoneView::List).into_string(),
            r#"<span hidden data-phone-view="list"></span>"#
        );
        assert_eq!(phone_view(PhoneView::Page).into_string(), "");
    }

    #[test]
    fn the_back_link_and_cancel_have_names() {
        let back = back_link("/", "Back to agents").into_string();
        assert!(back.contains("href=\"/\""));
        assert!(back.contains("<span class=\"visually-hidden\">Back to agents</span>"));
        let cancel = menu_cancel("menu-head").into_string();
        assert!(cancel.contains("popovertarget=\"menu-head\" popovertargetaction=\"hide\""));
        assert!(cancel.contains(">Cancel</button>"));
    }

    #[test]
    fn sheet_text_is_escaped() {
        let who = MenuWho {
            avatar: Avatar::Melissa,
            device: "<b>x</b>",
            status: Some("Needs permission"),
        };
        let html = menu_head(&who, "<i>n</i>").into_string();
        assert!(html.contains("&lt;b&gt;x&lt;/b&gt; · Needs permission"));
        assert!(html.contains("&lt;i&gt;n&lt;/i&gt;"));
        let sheet = SheetWho {
            avatar: Avatar::Melissa,
            sub: "<b>y</b>",
        };
        assert!(sheet_sub(&sheet).into_string().contains("&lt;b&gt;y"));
    }
}
