//! Sign-in pages and the People and Account settings (2026-11-21, PLAN 6c step 1). Built
//! from the existing pieces: `page`, `card`, fields, setting rows, inline buttons.

use maud::{html, Markup};

use super::{
    action_button, badge, card, command_block, link_button, notice, page, plain_action, region,
    row_menu, setting_card, setting_row, setting_row_full, setting_row_with, settings_form, Btn,
    Glyph, RowAction, Tone,
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
    let field = credential_input(name, label, kind, autocomplete, value, hint);
    if kind != "password" {
        return field;
    }
    html! {
        div.field-pw {
            (field)
            label.check.show-password {
                input type="checkbox" data-show-password;
                span { "Show password" }
            }
        }
    }
}

fn credential_input(
    name: &str,
    label: &str,
    kind: &str,
    autocomplete: &str,
    value: &str,
    hint: Option<&str>,
) -> Markup {
    html! {
        label.field {
            span { (label) }
            input.input type=(kind) name=(name) value=(value) autocomplete=(autocomplete)
                autocapitalize="none" spellcheck="false" required maxlength="256";
            @if let Some(h) = hint { small.hint { (h) } }
        }
    }
}

/// The password asked again inside a form ([`credential_field`], `current-password`) with the
/// server's verdict (COMPONENT-SPEC 2.16): with an `error` it is `aria-invalid`, takes the
/// focus, and the error line shows in place of the hint. Always empty.
pub fn password_checked(
    name: &str,
    label: &str,
    hint: Option<&str>,
    error: Option<&str>,
) -> Markup {
    html! {
        div.field-pw {
            label.field {
                span { (label) }
                input.input type="password" name=(name) value="" autocomplete="current-password"
                    autocapitalize="none" spellcheck="false" required maxlength="256"
                    aria-invalid=[error.map(|_| "true")] autofocus[error.is_some()];
                (super::field_note(hint, error))
            }
            label.check.show-password {
                input type="checkbox" data-show-password;
                span { "Show password" }
            }
        }
    }
}

/// Several password fields with one "Show passwords" switch for all of them (Change password,
/// 2026-11-23): (name, label, autocomplete, hint) each.
pub fn password_fields(fields: &[(&str, &str, &str, Option<&str>)]) -> Markup {
    html! {
        div.field-pw.stack {
            @for (name, label, autocomplete, hint) in fields {
                (credential_input(name, label, "password", autocomplete, "", *hint))
            }
            label.check.show-password {
                input type="checkbox" data-show-password;
                span { "Show passwords" }
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
                    @if let Some(e) = f.error { (notice(e, Tone::Caution)) }
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
                    (notice(text, Tone::Caution))
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
    /// The Change password form shows (its button, or a change that was refused).
    pub password_open: bool,
}

/// Where Account's Change password button leads: the page with the form open.
pub const ACCOUNT_PASSWORD_URL: &str = "/settings/account?password=1";

pub fn account_region(v: &AccountView<'_>) -> Markup {
    region(
        ACCOUNT_ID,
        html! {
            @if let Some((text, tone)) = v.flash { (notice(text, tone)) }
            (setting_card(None, html! {
                (setting_row(v.username, Some(v.role), html! {}))
                // Behind a button, opened in place (owner decision 3, 2026-11-23).
                @if v.password_open {
                    (setting_row_full(Some("Change password"), settings_form(
                        "/ui/account/password",
                        ACCOUNT_ID,
                        "Save password",
                        Some("/settings/account"),
                        password_fields(&[
                            ("current", "Current password", "current-password", None),
                            ("password", "New password", "new-password", Some("At least 8 characters.")),
                            ("again", "New password again", "new-password", None),
                        ]),
                    )))
                } @else {
                    (setting_row("Password", None,
                        link_button(Btn::secondary().sm(), ACCOUNT_PASSWORD_URL, "Change password")))
                }
            }))
            (setting_card(Some("Signed in on"), html! {
                @for s in &v.sessions {
                    (setting_row_with(
                        html! { (s.label) },
                        Some(html! { (s.when) }),
                        html! {
                            @if s.current { (badge("This browser", Tone::Neutral)) }
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
                    action_button(Btn::danger_quiet(), "/ui/account/sessions/all", ACCOUNT_ID, "Sign out all", "Sign out of every browser?"),
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

/// One person: name, role and since; Reset password and Remove behind one "⋯" (owner board
/// P2, 2026-11-23), so the row stays one line on a phone.
fn person_row(i: usize, p: &PersonRow<'_>) -> Markup {
    let remove_label = format!("Remove {}", p.username);
    let remove_ask = format!("Remove {}? They are signed out at once.", p.username);
    let mut items: Vec<RowAction<'_>> = Vec::new();
    if let Some(reset) = &p.reset {
        items.push(RowAction {
            label: "Reset password",
            icon: Glyph::Pencil,
            action: reset,
            target: PEOPLE_ID,
            confirm: None,
        });
    }
    if let Some(remove) = &p.remove {
        items.push(RowAction {
            label: &remove_label,
            icon: Glyph::Trash,
            action: remove,
            target: PEOPLE_ID,
            confirm: Some(&remove_ask),
        });
    }
    setting_row_with(
        html! { (p.username) },
        Some(html! { (p.role) " · " (p.since) }),
        html! {
            @if !items.is_empty() { (row_menu(&format!("person-menu-{i}"), p.username, &items)) }
        },
        None,
    )
}

pub fn people_region(v: &PeopleView<'_>) -> Markup {
    region(
        PEOPLE_ID,
        html! {
            @if let Some((text, tone)) = v.flash { (notice(text, tone)) }
            @if let Some((what, link)) = &v.new_link {
                (setting_card(Some(what), setting_row_full(None, html! {
                    p { "Send this link. It works once. You won't see it again." }
                    (command_block(link))
                })))
            }
            (setting_card(None, html! {
                @for (i, p) in v.people.iter().enumerate() { (person_row(i, p)) }
            }))
            (setting_card(Some("Invites"), html! {
                (setting_row(
                    "Invite someone",
                    Some("One link, one person, 24 hours."),
                    plain_action("/ui/people/invites", PEOPLE_ID, "Create link", false),
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
