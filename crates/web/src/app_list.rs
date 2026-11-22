//! Who sees which app, and the Apps list's groups (2026-11-22, owner design "Apps" A1–A4).
//!
//! **Whose app it is** ([`person_of`]): the person recorded at install time
//! (`melissa.io/installed-for`: whose message started the turn that installed it, or who called
//! the tool in the UI); else the person the agent that looks after it was made for (an install
//! during a scheduled job); else, for a workspace from before agents (`melissa.io/owner=owner`),
//! the owner, who was the only person then. Anything else is unknown.
//!
//! **Who sees it** ([`Looker::may_see`]): the owner sees every app; a member only the apps that
//! are theirs. Apps of unknown people are the owner's to sort out, so members don't see them
//! (nothing says they are theirs). This is checked on the server for the list, its JSON and
//! every app's details, logs and actions.
//!
//! **The list**: grouped by the app's kind (its spec's `kind`, [`AppKind`] order), each group
//! sorted by name. The owner can show one person's apps (`?by=`).

use std::collections::BTreeMap;

use melissa_agent::lead::icon::AgentIcon;
use melissa_appspec::AppKind;
use melissa_settings::prefs::Prefs;

use crate::state::{App, Keeper};

/// Who is looking: their account id and whether they are the owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Looker {
    pub id: String,
    pub owner: bool,
}

impl Looker {
    /// The person signed in for this request. Routers without the sign-in gate (tests, the
    /// harness's tools) have nobody signed in: they get the owner's view, as every other
    /// owner-only page does there.
    pub(crate) fn current() -> Self {
        match crate::auth::signed_in() {
            Some(p) => Self {
                owner: p.is_owner(),
                id: p.id,
            },
            None => Self {
                id: String::new(),
                owner: true,
            },
        }
    }

    /// May they see an app that is `person`'s (None: unknown)?
    pub(crate) fn may_see(&self, person: Option<&str>) -> bool {
        self.owner || (!self.id.is_empty() && person == Some(self.id.as_str()))
    }
}

/// What the list needs to know besides the apps: who made each agent, who the owner is.
#[derive(Debug, Clone, Default)]
pub(crate) struct Owners {
    /// Agent id → the person it was made for.
    pub creators: BTreeMap<String, String>,
    /// The owner's account id, if there is one.
    pub owner: Option<String>,
}

/// Everything the list needs besides the apps.
#[derive(Debug, Clone)]
pub(crate) struct AppsCx {
    pub looker: Looker,
    pub owners: Owners,
    /// Account id → username.
    pub names: BTreeMap<String, String>,
    pub prefs: Prefs,
    pub show: Show,
}

/// Whose app `app` is (see the module docs).
pub(crate) fn person_of(app: &App, owners: &Owners) -> Option<String> {
    if let Some(p) = &app.installed_for {
        return Some(p.clone());
    }
    match &app.keeper {
        Some(Keeper::Agent { id, .. }) => owners.creators.get(id).cloned(),
        Some(Keeper::Owner) => owners.owner.clone(),
        None => None,
    }
}

/// Whose apps the owner asked to see (`?by=`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Show {
    Everyone,
    /// One person's (their account id).
    Person(String),
    /// Apps nobody is recorded for.
    Unknown,
}

impl Show {
    /// From `?by=`: `everyone` (or nothing), `me`, `unknown` or a person's id. Members always
    /// see their own apps, whatever they ask for.
    pub(crate) fn parse(by: Option<&str>, looker: &Looker) -> Self {
        if !looker.owner {
            return Self::Person(looker.id.clone());
        }
        match by.map(str::trim) {
            None | Some("" | "everyone") => Self::Everyone,
            Some("me") => Self::Person(looker.id.clone()),
            Some("unknown") => Self::Unknown,
            Some(id) if melissa_appspec::valid_person_id(id) => Self::Person(id.to_string()),
            Some(_) => Self::Everyone,
        }
    }

    /// The `?by=` value that asks for this (`me` for the looker's own).
    pub(crate) fn key(&self, looker: &Looker) -> String {
        match self {
            Self::Everyone => "everyone".into(),
            Self::Unknown => "unknown".into(),
            Self::Person(id) if *id == looker.id => "me".into(),
            Self::Person(id) => id.clone(),
        }
    }

    fn admits(&self, person: Option<&str>) -> bool {
        match self {
            Self::Everyone => true,
            Self::Unknown => person.is_none(),
            Self::Person(id) => person == Some(id.as_str()),
        }
    }
}

/// One app as the list shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Listed<'a> {
    pub app: &'a App,
    /// Whose it is (None: unknown).
    pub person: Option<String>,
    pub kind: AppKind,
    pub icon: AgentIcon,
}

/// An app's icon: the one picked in the UI (known names only), else the one from its name.
pub(crate) fn icon_of(name: &str, prefs: &Prefs) -> AgentIcon {
    prefs
        .app_icons
        .get(name)
        .and_then(|i| AgentIcon::new(&i.shape, &i.color))
        .unwrap_or_else(|| AgentIcon::for_app(name))
}

/// The apps `looker` may see, with whose each is.
pub(crate) fn visible<'a>(
    apps: &'a [App],
    looker: &Looker,
    owners: &Owners,
    prefs: &Prefs,
) -> Vec<Listed<'a>> {
    apps.iter()
        .map(|app| Listed {
            person: person_of(app, owners),
            kind: app.kind.unwrap_or(AppKind::Other),
            icon: icon_of(&app.name, prefs),
            app,
        })
        .filter(|l| looker.may_see(l.person.as_deref()))
        .collect()
}

/// The ones `show` asks for.
pub(crate) fn shown<'a>(listed: &[Listed<'a>], show: &Show) -> Vec<Listed<'a>> {
    listed
        .iter()
        .filter(|l| show.admits(l.person.as_deref()))
        .cloned()
        .collect()
}

/// Grouped by kind (in [`AppKind`] order), each group by display name.
pub(crate) fn groups<'a>(listed: Vec<Listed<'a>>) -> Vec<(AppKind, Vec<Listed<'a>>)> {
    let mut by: BTreeMap<AppKind, Vec<Listed<'a>>> = BTreeMap::new();
    for l in listed {
        by.entry(l.kind).or_default().push(l);
    }
    by.into_iter()
        .map(|(kind, mut apps)| {
            apps.sort_by_key(|l| (l.app.display_name().to_lowercase(), l.app.name.clone()));
            (kind, apps)
        })
        .collect()
}

/// A group's heading.
pub(crate) fn kind_words(kind: AppKind) -> &'static str {
    match kind {
        AppKind::Games => "Games",
        AppKind::Media => "Media",
        AppKind::Home => "Home",
        AppKind::Files => "Files",
        AppKind::Network => "Network",
        AppKind::Developer => "Developer",
        AppKind::Other => "Other",
    }
}

/// The people the owner can show (besides Everyone and Me): everyone else who has an app,
/// by name, then "Unknown" when some apps are nobody's. `names`: account id → username.
pub(crate) fn filter_people(
    listed: &[Listed<'_>],
    looker: &Looker,
    names: &BTreeMap<String, String>,
) -> Vec<(Show, String)> {
    let mut people: BTreeMap<String, String> = BTreeMap::new();
    let mut unknown = false;
    for l in listed {
        match &l.person {
            Some(p) if *p == looker.id => {}
            Some(p) => {
                people
                    .entry(p.clone())
                    .or_insert_with(|| person_words(p, names));
            }
            None => unknown = true,
        }
    }
    let mut out: Vec<(Show, String)> = people
        .into_iter()
        .map(|(id, name)| (Show::Person(id), name))
        .collect();
    out.sort_by_key(|(_, name)| name.to_lowercase());
    if unknown {
        out.push((Show::Unknown, "Unknown".into()));
    }
    out
}

/// A person's name for the list: their username, "Removed person" for an account that is
/// gone.
pub(crate) fn person_words(id: &str, names: &BTreeMap<String, String>) -> String {
    names
        .get(id)
        .cloned()
        .unwrap_or_else(|| "Removed person".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str, kind: Option<AppKind>, for_: Option<&str>, keeper: Option<Keeper>) -> App {
        App {
            name: name.into(),
            phase: "ready".into(),
            url: None,
            port: None,
            keeper,
            other_port: None,
            kind,
            installed_for: for_.map(str::to_string),
        }
    }

    fn owner() -> Looker {
        Looker {
            id: "p-owner".into(),
            owner: true,
        }
    }

    fn member(id: &str) -> Looker {
        Looker {
            id: id.into(),
            owner: false,
        }
    }

    fn owners() -> Owners {
        Owners {
            creators: [("d-1.a1".to_string(), "p-ada".to_string())].into(),
            owner: Some("p-owner".into()),
        }
    }

    fn apps() -> Vec<App> {
        vec![
            app("minecraft", Some(AppKind::Games), Some("p-ben"), None),
            app("luanti", Some(AppKind::Games), Some("p-ada"), None),
            app("jellyfin", Some(AppKind::Media), Some("p-owner"), None),
            // Installed by a job of Ada's helper: hers.
            app(
                "syncthing",
                Some(AppKind::Files),
                None,
                Some(Keeper::Agent {
                    id: "d-1.a1".into(),
                    name: "Files".into(),
                }),
            ),
            // From before agents: the owner's.
            app("nextcloud", None, None, Some(Keeper::Owner)),
            // From before people were recorded: nobody's.
            app("forgejo", Some(AppKind::Developer), None, None),
        ]
    }

    fn names(l: &[Listed<'_>]) -> Vec<String> {
        l.iter().map(|l| l.app.name.clone()).collect()
    }

    #[test]
    fn whose_an_app_is() {
        let o = owners();
        let a = apps();
        let who: Vec<Option<String>> = a.iter().map(|a| person_of(a, &o)).collect();
        assert_eq!(
            who,
            vec![
                Some("p-ben".into()),
                Some("p-ada".into()),
                Some("p-owner".into()),
                Some("p-ada".into()),
                Some("p-owner".into()),
                None,
            ]
        );
    }

    #[test]
    fn the_owner_sees_every_app_and_a_member_only_theirs() {
        let a = apps();
        let p = Prefs::default();
        assert_eq!(visible(&a, &owner(), &owners(), &p).len(), 6);
        assert_eq!(
            names(&visible(&a, &member("p-ada"), &owners(), &p)),
            ["luanti", "syncthing"]
        );
        assert_eq!(
            names(&visible(&a, &member("p-ben"), &owners(), &p)),
            ["minecraft"]
        );
        // Unknown apps stay with the owner; a member without apps sees none.
        assert!(visible(&a, &member("p-cy"), &owners(), &p).is_empty());
        // An empty id never matches an unknown app.
        assert!(!member("").may_see(None));
    }

    #[test]
    fn the_filter_is_the_owners_only() {
        let o = owner();
        assert_eq!(Show::parse(None, &o), Show::Everyone);
        assert_eq!(Show::parse(Some("me"), &o), Show::Person("p-owner".into()));
        assert_eq!(Show::parse(Some("p-ben"), &o), Show::Person("p-ben".into()));
        assert_eq!(Show::parse(Some("unknown"), &o), Show::Unknown);
        assert_eq!(Show::parse(Some("<b>"), &o), Show::Everyone);
        let m = member("p-ada");
        // A member asking for someone else's apps still gets their own.
        assert_eq!(Show::parse(Some("p-ben"), &m), Show::Person("p-ada".into()));
        assert_eq!(
            Show::parse(Some("everyone"), &m),
            Show::Person("p-ada".into())
        );
        assert_eq!(Show::Person("p-owner".into()).key(&o), "me");
        assert_eq!(Show::Person("p-ben".into()).key(&o), "p-ben");
    }

    #[test]
    fn filtering_and_grouping() {
        let a = apps();
        let p = Prefs::default();
        let all = visible(&a, &owner(), &owners(), &p);
        assert_eq!(
            names(&shown(&all, &Show::Person("p-ada".into()))),
            ["luanti", "syncthing"]
        );
        assert_eq!(names(&shown(&all, &Show::Unknown)), ["forgejo"]);
        assert_eq!(
            names(&shown(&all, &Show::Person("p-owner".into()))),
            ["jellyfin", "nextcloud"]
        );
        let g = groups(all.clone());
        let kinds: Vec<AppKind> = g.iter().map(|(k, _)| *k).collect();
        assert_eq!(
            kinds,
            [
                AppKind::Games,
                AppKind::Media,
                AppKind::Files,
                AppKind::Developer,
                AppKind::Other
            ]
        );
        assert_eq!(names(&g[0].1), ["luanti", "minecraft"]);
        let people: BTreeMap<String, String> = [
            ("p-ada".to_string(), "ada".to_string()),
            ("p-owner".to_string(), "owner".to_string()),
        ]
        .into();
        let filter = filter_people(&all, &owner(), &people);
        assert_eq!(
            filter,
            vec![
                (Show::Person("p-ada".into()), "ada".to_string()),
                (Show::Person("p-ben".into()), "Removed person".to_string()),
                (Show::Unknown, "Unknown".to_string()),
            ]
        );
    }

    #[test]
    fn a_picked_icon_wins_over_the_derived_one() {
        let mut p = Prefs::default();
        assert_eq!(icon_of("minecraft", &p), AgentIcon::for_app("minecraft"));
        p.app_icons.insert(
            "minecraft".into(),
            melissa_settings::prefs::AppIconPref {
                shape: "cube".into(),
                color: "lime".into(),
            },
        );
        assert_eq!(
            icon_of("minecraft", &p),
            AgentIcon::new("cube", "lime").unwrap()
        );
        p.app_icons.insert(
            "jellyfin".into(),
            melissa_settings::prefs::AppIconPref {
                shape: "<svg>".into(),
                color: "lime".into(),
            },
        );
        assert_eq!(icon_of("jellyfin", &p), AgentIcon::for_app("jellyfin"));
    }
}
