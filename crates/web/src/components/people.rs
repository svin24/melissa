//! Sign-in pages and the People and Account settings (2026-11-21, PLAN 6c step 1). Built
//! from the existing pieces: `page`, `card`, fields, setting rows, inline buttons.

use maud::{html, Markup};

use super::{
    action_button, card, command_block, notice, page, page_header, plain_action, region,
    setting_card, setting_row, setting_row_with, Btn, Tone,
};

/// The CSRF token as a hidden form field: the first field of every POST form (multipart
/// forms carry it as their first part). Empty outside a signed-in request.
pub fn csrf_field() -> Markup {
    match crate::auth::csrf_token() {
        Some(t) => html! { input type="hidden" name="_csrf" value=(t); },
        None => html! {},
    }
}

/// A username or password field with the browser's autofill hint (`username`,
/// `current-password`, `new-password`, `one-time-code`); otherwise like `text_field`.
/// A password field gets a "Show password" switch (melissa.js; hidden without script).
pub fn credential_field(
    name: &str,
    label: &str,
    kind: &str,
    autocomplete: &str,
    value: &str,
    hint: Option<&str>,
) -> Markup {
    let field = html! {
        label.field {
            span { (label) }
            input.input type=(kind) name=(name) value=(value) autocomplete=(autocomplete)
                autocapitalize="none" spellcheck="false" required maxlength="256";
            @if let Some(h) = hint { small.hint { (h) } }
        }
    };
    if kind != "password" {
        return field;
    }
    html! {
        div.field-pw {
            (field)
            label.show-password {
                input type="checkbox" data-show-password;
                span { "Show password" }
            }
        }
    }
}

/// A page with one card holding one form that posts normally (sign in, claim, invite,
/// password again). `next`: where to go afterwards (a hidden field).
pub struct AuthForm<'a> {
    pub title: &'a str,
    pub intro: Option<&'a str>,
    pub error: Option<&'a str>,
    pub action: &'a str,
    pub next: Option<&'a str>,
    pub fields: Markup,
    pub submit: &'a str,
}

pub fn auth_page(f: &AuthForm<'_>) -> Markup {
    page(
        f.title,
        html! {
            div.auth-card {
                (card(f.title, html! {
                    @if let Some(i) = f.intro { p { (i) } }
                    @if let Some(e) = f.error { (notice(e, Tone::Error)) }
                    form.stack method="post" action=(f.action) {
                        (csrf_field())
                        @if let Some(n) = f.next { input type="hidden" name="next" value=(n); }
                        (f.fields)
                        div { button.(Btn::primary()) type="submit" { (f.submit) } }
                    }
                }))
            }
        },
    )
}

/// A one-line page for a link that no longer works.
pub fn auth_dead_end(title: &str, text: &str) -> Markup {
    page(
        title,
        html! {
            div.auth-card {
                (card(title, html! {
                    (notice(text, Tone::Error))
                    a href="/login" { "Go to sign in" }
                }))
            }
        },
    )
}

/// The Sign out item for a menu: a form with one menu button.
pub fn sign_out_item() -> Markup {
    html! {
        form method="post" action="/logout" {
            (csrf_field())
            button.menu-item type="submit" { (super::glyph(super::Glyph::Close)) span { "Sign out" } }
        }
    }
}

// --- Settings → Account -------------------------------------------------------------------

pub const ACCOUNT_ID: &str = "account";

/// One session in "Signed in on".
pub struct SessionRow<'a> {
    pub label: &'a str,
    /// "Last used 3 hours ago".
    pub when: String,
    pub current: bool,
    /// Sign out that session (None for this browser).
    pub revoke: Option<String>,
}

pub struct AccountView<'a> {
    pub username: &'a str,
    pub role: &'a str,
    pub sessions: Vec<SessionRow<'a>>,
    pub flash: Option<(&'a str, Tone)>,
}

pub fn account_region(v: &AccountView<'_>) -> Markup {
    region(
        ACCOUNT_ID,
        html! {
            (page_header("Account", Some("Your sign-in on this Melissa.")))
            @if let Some((text, tone)) = v.flash { (notice(text, tone)) }
            (setting_card(None, html! {
                (setting_row("Username", None, html! { (v.username) }))
                (setting_row("Role", None, html! { (v.role) }))
            }))
            (setting_card(Some("Change password"), html! {
                div.setting-row {
                    form.stack.settings-form method="post" action="/ui/account/password"
                        hx-post="/ui/account/password" hx-target={ "#" (ACCOUNT_ID) } hx-swap="outerHTML" {
                        (csrf_field())
                        (credential_field("current", "Current password", "password", "current-password", "", None))
                        (credential_field("password", "New password", "password", "new-password", "", Some("At least 8 characters.")))
                        (credential_field("again", "New password again", "password", "new-password", "", None))
                        div { button.(Btn::secondary()) type="submit" { "Change password" } }
                    }
                }
            }))
            (setting_card(Some("Signed in on"), html! {
                @for s in &v.sessions {
                    (setting_row_with(
                        html! { (s.label) },
                        Some(html! { (s.when) }),
                        html! {
                            @if s.current { span.quiet { "This browser" } }
                            @if let Some(action) = &s.revoke {
                                (plain_action(action, ACCOUNT_ID, "Sign out", false))
                            }
                        },
                        None,
                    ))
                }
                (setting_row_with(
                    html! { "Sign out everywhere" },
                    Some(html! { "Every browser, this one too." }),
                    action_button("/ui/account/sessions/all", ACCOUNT_ID, "Sign out everywhere", "Sign out of every browser?"),
                    None,
                ))
            }))
        },
    )
}

// --- Settings → People (owner) ------------------------------------------------------------

pub const PEOPLE_ID: &str = "people";

pub struct PersonRow<'a> {
    pub username: &'a str,
    pub role: &'a str,
    /// "Joined 3 days ago".
    pub since: String,
    /// Member actions: reset password, remove.
    pub reset: Option<String>,
    pub remove: Option<String>,
}

pub struct InviteRow {
    /// "Invite" or "Password reset for bob".
    pub what: String,
    /// "Expires in 23 hours".
    pub expires: String,
    pub revoke: String,
}

pub struct PeopleView<'a> {
    pub people: Vec<PersonRow<'a>>,
    pub invites: Vec<InviteRow>,
    /// A link made just now: (what it is, the link). Shown once.
    pub new_link: Option<(String, String)>,
    pub flash: Option<(&'a str, Tone)>,
}

pub fn people_region(v: &PeopleView<'_>) -> Markup {
    region(
        PEOPLE_ID,
        html! {
            (page_header("People", Some("Who can sign in to this Melissa. You are the owner; members join with an invite link.")))
            @if let Some((text, tone)) = v.flash { (notice(text, tone)) }
            @if let Some((what, link)) = &v.new_link {
                (setting_card(Some(what), html! {
                    div.setting-row {
                        div.stack {
                            p { "Send this link. It works once. You won't see it again." }
                            (command_block(link))
                        }
                    }
                }))
            }
            (setting_card(None, html! {
                @for p in &v.people {
                    (setting_row_with(
                        html! { (p.username) },
                        Some(html! { (p.role) " · " (p.since) }),
                        html! {
                            @if let Some(reset) = &p.reset {
                                (plain_action(reset, PEOPLE_ID, "Reset password", false))
                            }
                            @if let Some(remove) = &p.remove {
                                (action_button(remove, PEOPLE_ID, "Remove", &format!("Remove {}? They are signed out at once.", p.username)))
                            }
                        },
                        None,
                    ))
                }
            }))
            (setting_card(Some("Invites"), html! {
                (setting_row(
                    "Invite someone",
                    Some("A link to join as a member. It works once, for 24 hours."),
                    plain_action("/ui/people/invites", PEOPLE_ID, "Create invite link", true),
                ))
                @for i in &v.invites {
                    (setting_row_with(
                        html! { (i.what) },
                        Some(html! { (i.expires) }),
                        plain_action(&i.revoke, PEOPLE_ID, "Withdraw", false),
                        None,
                    ))
                }
            }))
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_pages_post_plain_forms_and_escape() {
        let out = auth_page(&AuthForm {
            title: "Sign in",
            intro: None,
            error: Some("<b>no</b>"),
            action: "/login",
            next: Some("/settings?x=\"1\""),
            fields: credential_field("username", "Username", "text", "username", "a<b", None),
            submit: "Sign in",
        })
        .into_string();
        assert!(out.contains(r#"<form class="stack" method="post" action="/login">"#));
        assert!(out.contains("&lt;b&gt;no&lt;/b&gt;"));
        assert!(out.contains("value=\"a&lt;b\""));
        assert!(out.contains(r#"autocomplete="username""#));
        assert!(out.contains("&quot;1&quot;"));
        // Outside a signed-in request there is no token to add.
        assert!(!out.contains("_csrf"));
    }
}
