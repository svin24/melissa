//! The agent workspace's data (2026-11-20, owner design): which group each agent is in and
//! what its sidebar row says, its menu's links, the chat header's status and the details
//! panel's recent activity. Words only; the markup is in `components::agent`.
//!
//! Groups, top to bottom (an agent is in exactly one, the first that applies; 2026-11-21):
//! - **Needs attention**: an approval it asked for waits, its last turn failed, or an app it
//!   looks after has a problem.
//! - **Pinned**: Melissa (always) and the helpers the user pinned (working or not).
//! - **Working**: a turn runs (a job or a message); shown last, at the bottom.
//! - **Idle**: everything else, no heading, most recently active first; shown between Pinned
//!   and Working, so an agent that finishes moves back up.

use maud::Markup;
use melissa_agent::{
    lead::{
        agents::AgentView,
        events::{EventLog, EventRecord, LeadEvent},
        icon::AgentIcon,
    },
    tools::Principal,
};

use crate::{
    components::{
        agent_groups, sidebar, ActivityLine, AgentMenu, Avatar, GroupKind, HeadStatus, Health,
        MenuWho, Nav, SideAgent, SideDevice, SideGroup, Sidebar, UpdateHint,
    },
    feed::{approval_words, tool_words, Clock, Lookup},
    pages::{agent_href, MELISSA_CHAT_URL},
    state::{phase_words, Apps, ClusterSnapshot, Keeper},
    ui::Ui,
};

/// Where an agent's menu items lead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuUrls {
    pub settings: String,
    pub rename: Option<String>,
    pub icon: Option<String>,
    pub pin: Option<(String, bool)>,
    pub fresh: String,
    pub earlier: Option<String>,
    pub delete: Option<String>,
}

impl MenuUrls {
    /// The links of `key`'s menu (`lead` or a helper's id).
    pub fn of(key: &str, pinned: bool, earlier: bool) -> Self {
        if key == "lead" {
            return Self {
                settings: "/settings/agents".into(),
                rename: Some(format!("{}#agent-name", agent_settings_href("lead"))),
                icon: None,
                pin: None,
                fresh: "/ui/fresh-start".into(),
                earlier: earlier.then(|| "/chats".into()),
                delete: None,
            };
        }
        let page = agent_settings_href(key);
        Self {
            rename: Some(format!("{page}#agent-name")),
            icon: Some(format!("{page}#agent-icon")),
            pin: Some((format!("/ui/agents/{key}/pin"), pinned)),
            fresh: format!("/ui/agents/{key}/fresh-start"),
            earlier: None,
            delete: Some(format!("{page}#agent-delete")),
            settings: page,
        }
    }

    pub fn menu<'a>(&'a self, name: &'a str) -> AgentMenu<'a> {
        AgentMenu {
            name,
            settings: &self.settings,
            rename: self.rename.as_deref(),
            icon: self.icon.as_deref(),
            pin: self.pin.as_ref().map(|(a, p)| (a.as_str(), *p)),
            fresh: &self.fresh,
            earlier: self.earlier.as_deref(),
            delete: self.delete.as_deref(),
            who: None,
        }
    }
}

/// A helper's page in Settings → Agents (`lead`: the lead agent's, its name only).
pub fn agent_settings_href(id: &str) -> String {
    format!("/settings/agents/{id}")
}

/// One agent as the sidebar shows it (owned; compared to notice changes for live updates).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SideEntry {
    /// `lead` or the helper's id.
    pub key: String,
    pub href: String,
    pub name: String,
    /// None: Melissa.
    pub icon: Option<AgentIcon>,
    pub group: GroupKind,
    pub status: Option<String>,
    pub sub: Option<String>,
    pub working: bool,
    pub head: Option<HeadStatus>,
    pub urls: MenuUrls,
    /// When it last did or heard something (unix seconds): orders the idle agents.
    pub last_active: u64,
}

impl SideEntry {
    pub fn avatar(&self) -> Avatar<'_> {
        match &self.icon {
            None => Avatar::Melissa,
            Some(i) => Avatar::Agent {
                shape: &i.shape,
                color: &i.color,
            },
        }
    }
}

/// The sidebar's data.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SideData {
    pub entries: Vec<SideEntry>,
    /// (name, health, status) of each device; the first is this one.
    pub devices: Vec<(String, Health, &'static str)>,
    /// Pending approvals in all.
    pub waiting: usize,
    /// The name the owner gave this device in setup (display only), if any.
    pub this_device: Option<String>,
}

impl SideData {
    /// This device's name: the one the owner gave it, else the cluster's ("melissa"), else
    /// "This device" when it isn't known yet.
    pub fn device(&self) -> &str {
        self.this_device.as_deref().unwrap_or_else(|| {
            self.devices
                .first()
                .map_or("This device", |(n, _, _)| n.as_str())
        })
    }

    /// The lead agent's display name ("Melissa" until the user renames it).
    pub fn lead_name(&self) -> &str {
        self.entry("lead")
            .map_or(melissa_agent::lead::registry::LEAD_DEFAULT_NAME, |e| {
                e.name.as_str()
            })
    }

    pub fn entry(&self, key: &str) -> Option<&SideEntry> {
        self.entries.iter().find(|e| e.key == key)
    }
}

/// The sidebar for a page (`current`: the agent whose chat it is, `lead` or an id).
pub fn sidebar_markup(
    nav: Nav,
    side: &SideData,
    current: Option<&str>,
    update: &UpdateHint,
) -> Markup {
    let devices: Vec<SideDevice<'_>> = side
        .devices
        .iter()
        .enumerate()
        .map(|(i, (name, health, status))| SideDevice {
            name,
            health: *health,
            status,
            current: i == 0,
        })
        .collect();
    let groups = side_groups(side, current);
    sidebar(&Sidebar {
        devices: &devices,
        groups: &groups,
        current: nav,
        update,
        new_agent: NEW_AGENT_URL,
        waiting: side.waiting,
    })
}

/// Where the sidebar's "+" leads.
pub const NEW_AGENT_URL: &str = "/settings/agents/new";

/// The agent groups alone, as a live update.
pub fn groups_update(side: &SideData, current: Option<&str>) -> Markup {
    agent_groups(&side_groups(side, current), side.waiting, true)
}

/// The groups in the order they are shown.
const GROUP_ORDER: [GroupKind; 4] = [
    GroupKind::Attention,
    GroupKind::Pinned,
    GroupKind::Idle,
    GroupKind::Working,
];

/// The entries of one group in the order they are shown: idle agents most recently active
/// first (then by name, so the order is stable), the others as listed.
fn group_entries(side: &SideData, kind: GroupKind) -> Vec<&SideEntry> {
    let mut list: Vec<&SideEntry> = side.entries.iter().filter(|e| e.group == kind).collect();
    if kind == GroupKind::Idle {
        list.sort_by(|a, b| {
            b.last_active
                .cmp(&a.last_active)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
    }
    list
}

fn side_groups<'a>(side: &'a SideData, current: Option<&str>) -> Vec<SideGroup<'a>> {
    GROUP_ORDER
        .into_iter()
        .map(|kind| SideGroup {
            kind,
            agents: group_entries(side, kind)
                .into_iter()
                .map(|e| SideAgent {
                    key: &e.key,
                    href: &e.href,
                    name: &e.name,
                    avatar: e.avatar(),
                    star: e.icon.is_none(),
                    status: e.status.as_deref(),
                    sub: e.sub.as_deref(),
                    progress: e.working.then_some(None),
                    current: current == Some(e.key.as_str()),
                    menu: AgentMenu {
                        who: Some(MenuWho {
                            avatar: e.avatar(),
                            device: side.device(),
                            status: e.status.as_deref(),
                        }),
                        ..e.urls.menu(&e.name)
                    },
                })
                .collect(),
        })
        .collect()
}

/// What the agent is doing now, from its log: the running tool call's words, or None.
fn activity_of(log: &EventLog) -> Option<String> {
    let from = log.last_seq().saturating_sub(200);
    let page = log.since(from, 200);
    let mut done: Vec<&str> = Vec::new();
    for r in page.events.iter().rev() {
        match &r.event {
            LeadEvent::UserMessage { .. } => break,
            LeadEvent::ToolCall {
                id, title, status, ..
            } => {
                if status == "done" || status == "failed" {
                    done.push(id);
                    continue;
                }
                if done.contains(&id.as_str()) {
                    continue;
                }
                let w = tool_words(title);
                if !w.hidden && !w.unknown {
                    return Some(w.doing.to_string());
                }
            }
            _ => {}
        }
    }
    None
}

impl Ui {
    /// The sidebar's data now (from the agents, the approvals waiting and the apps).
    pub(crate) fn side_data(&self, snap: &ClusterSnapshot) -> SideData {
        self.side_data_with(snap, !self.lead.past_chats().is_empty())
    }

    /// [`Ui::side_data`] when it is known whether Melissa has earlier chats (`earlier`).
    pub(crate) fn side_data_with(&self, snap: &ClusterSnapshot, earlier: bool) -> SideData {
        let mut devices: Vec<(String, Health, &'static str)> = snap
            .devices
            .iter()
            .map(|d| {
                let (h, w) = match d.ready {
                    Some(true) => (Health::Ok, "Online"),
                    Some(false) => (Health::Problem, "Not ready"),
                    None => (Health::Idle, "Status unknown"),
                };
                (d.name.clone(), h, w)
            })
            .collect();
        // The owner's name for this device (setup) shows wherever the UI names it; the
        // cluster node keeps its own name.
        let this_device = self.settings.device_name();
        if let (Some(name), Some(first)) = (&this_device, devices.first_mut()) {
            first.0 = name.clone();
        }
        let device = this_device.clone().unwrap_or_else(|| {
            devices
                .first()
                .map_or_else(|| "This device".to_string(), |d| d.0.clone())
        });
        let pending = self.lead.confirmations().pending();
        let lead_task = self.lead.task_id();
        let agents = self.lead.agents();
        let asking: Vec<Option<String>> = pending
            .iter()
            .map(|c| {
                if c.task == lead_task {
                    Some("lead".to_string())
                } else {
                    agents.agent_of_task(c.task)
                }
            })
            .collect();
        let apps: &[crate::state::App] = match &snap.apps {
            Apps::Known { installed } => installed,
            _ => &[],
        };
        let mut entries = Vec::new();

        // Melissa: always pinned (unless she needs attention).
        let lead_busy = self.lead.status().turn.is_some();
        let lead_asks = asking.iter().any(|a| a.as_deref() == Some("lead"));
        let lead_failed = self.lead.failed();
        let (group, status, head) = if lead_asks {
            (
                GroupKind::Attention,
                Some("Permission needed".to_string()),
                Some(HeadStatus::NeedsPermission),
            )
        } else if lead_failed {
            (
                GroupKind::Attention,
                Some("Couldn't finish".to_string()),
                Some(HeadStatus::Failed),
            )
        } else if lead_busy {
            (
                GroupKind::Pinned,
                Some(activity_of(self.lead.events()).unwrap_or_else(|| "Working on it".into())),
                Some(HeadStatus::Working),
            )
        } else {
            (GroupKind::Pinned, None, None)
        };
        entries.push(SideEntry {
            key: "lead".into(),
            // Not `/`: on phones `/` is the agent list (components::mobile).
            href: MELISSA_CHAT_URL.into(),
            name: agents.lead_name(),
            icon: None,
            group,
            status,
            sub: Some("Lead agent".into()),
            working: lead_busy,
            head,
            urls: MenuUrls::of("lead", true, earlier),
            last_active: 0,
        });

        for a in agents.list() {
            entries.push(self.helper_entry(&a, &asking, apps, &device));
        }
        SideData {
            entries,
            devices,
            waiting: pending.len(),
            this_device,
        }
    }

    fn helper_entry(
        &self,
        a: &AgentView,
        asking: &[Option<String>],
        apps: &[crate::state::App],
        device: &str,
    ) -> SideEntry {
        let mine: Vec<&crate::state::App> = apps
            .iter()
            .filter(|app| matches!(&app.keeper, Some(Keeper::Agent { id, .. }) if *id == a.id))
            .collect();
        let problem = mine
            .iter()
            .find(|app| phase_words(&app.phase).0 == Health::Problem);
        let asks = asking.iter().any(|x| x.as_deref() == Some(a.id.as_str()));
        let working = a.status == "working";
        let (group, status, head) = if asks {
            (
                GroupKind::Attention,
                Some("Permission needed".to_string()),
                Some(HeadStatus::NeedsPermission),
            )
        } else if a.failed {
            (
                GroupKind::Attention,
                Some("Couldn't finish".to_string()),
                Some(HeadStatus::Failed),
            )
        } else if let Some(app) = problem {
            (
                GroupKind::Attention,
                Some(format!("{} needs a look", app.display_name())),
                Some(HeadStatus::Failed),
            )
        } else if working {
            let doing = self
                .lead
                .agents()
                .events_of(&a.id)
                .ok()
                .and_then(|log| activity_of(&log))
                .or_else(|| a.job.as_deref().map(|j| shorten(j, 60)))
                .unwrap_or_else(|| "Working on it".into());
            let group = if a.pinned {
                GroupKind::Pinned
            } else {
                GroupKind::Working
            };
            (group, Some(doing), Some(HeadStatus::Working))
        } else if a.pinned {
            (GroupKind::Pinned, None, None)
        } else {
            (GroupKind::Idle, None, None)
        };
        let sub = match mine.first() {
            Some(app) => format!("{device} · {}", app.display_name()),
            None if a.status == "stopped" => "Stopped".into(),
            None => "Helper".into(),
        };
        SideEntry {
            key: a.id.clone(),
            href: agent_href(&a.id),
            name: a.name.clone(),
            icon: Some(a.icon.clone()),
            group,
            status,
            sub: Some(sub),
            working,
            head,
            urls: MenuUrls::of(&a.id, a.pinned, false),
            last_active: self
                .lead
                .agents()
                .last_active(&a.id)
                .unwrap_or(a.created_at),
        }
    }

    /// The last notable things in `owner`'s conversation and file box, newest first: what it
    /// did (tool calls that finished), what it asked for, what the owner decided, files
    /// added, helpers and jobs (Melissa's). `may_see`: whether the person looking may see an
    /// app (by its name); lines about other apps are left out.
    pub(crate) fn recent_activity(
        &self,
        owner: &Principal,
        log: &EventLog,
        clock: &Clock,
        may_see: &dyn Fn(&str) -> bool,
    ) -> Vec<ActivityLine> {
        let page = log.since(0, EventLog::CAPACITY);
        let mut lines: Vec<(u64, String, bool)> = activity_from(&page.events, self, may_see);
        for f in self.lead.files().list(owner) {
            lines.push((f.added, format!("Added {}", shorten(&f.name, 40)), false));
        }
        lines.sort_by_key(|l| std::cmp::Reverse(l.0));
        lines.dedup_by(|a, b| a.1 == b.1 && a.0 / 60 == b.0 / 60);
        lines.truncate(RECENT_LINES);
        lines
            .into_iter()
            .map(|(ts, text, attention)| ActivityLine {
                when: clock.short_when(ts),
                text,
                attention,
            })
            .collect()
    }
}

/// Lines in the recent activity section.
const RECENT_LINES: usize = 6;

/// Tools whose outcome has an event of its own in the log (and its own activity line).
const OWN_LINES: [&str; 3] = ["reassign_workspace", "delete_agent", "message_agent"];

/// The notable events of a conversation as (time, words, attention).
pub(crate) fn activity_from(
    events: &[EventRecord],
    look: &dyn Lookup,
    may_see: &dyn Fn(&str) -> bool,
) -> Vec<(u64, String, bool)> {
    let mut out = Vec::new();
    for r in events {
        let line = match &r.event {
            // Handing an app over and deleting a helper have their own lines (below), which
            // say who and what.
            LeadEvent::ToolCall { title, status, .. }
                if status == "done" && !OWN_LINES.iter().any(|t| title.contains(t)) =>
            {
                let w = tool_words(title);
                (!w.hidden && !w.unknown).then(|| (w.done.to_string(), false))
            }
            LeadEvent::WorkspaceReassigned { workspace, .. } if !may_see(workspace) => None,
            LeadEvent::WorkspaceReassigned { workspace, to, .. } => {
                let who = look.agent_name(to).unwrap_or_else(|| to.clone());
                Some((
                    format!(
                        "{who} looks after {} now",
                        crate::feed::display_app(workspace)
                    ),
                    false,
                ))
            }
            LeadEvent::AgentDeleted { name, .. } => {
                Some((format!("Deleted helper: {name}"), false))
            }
            LeadEvent::AgentMessaged { agent, name } => {
                let name = look.agent_name(agent).unwrap_or_else(|| name.clone());
                Some((format!("Messaged {name}"), false))
            }
            LeadEvent::HelperReport {
                agent,
                name,
                message: Some(_),
                ..
            } => {
                let name = look.agent_name(agent).unwrap_or_else(|| name.clone());
                Some((format!("Message from {name}"), false))
            }
            LeadEvent::ConfirmationNeeded {
                id, tool, reason, ..
            } => {
                let a = approval_words(*id, tool, reason, None, look);
                Some((format!("Asked: {}", a.title.trim_end_matches('?')), true))
            }
            LeadEvent::ConfirmationResolved { outcome, .. } => match outcome.as_str() {
                "approved" => Some(("You allowed a request".to_string(), false)),
                "denied" => Some(("You declined a request".to_string(), false)),
                _ => None,
            },
            LeadEvent::AgentCreated { agent, name, .. } => {
                // The current name: the helper may have been renamed since.
                let name = look.agent_name(agent).unwrap_or_else(|| name.clone());
                Some((format!("New helper: {name}"), false))
            }
            LeadEvent::AgentFinished { name, status, .. } => Some(match status.as_str() {
                "succeeded" => (format!("{name} finished a job"), false),
                "cancelled" => (format!("{name} was stopped"), false),
                _ => (format!("{name} couldn't finish a job"), true),
            }),
            LeadEvent::FreshStart => Some(("Fresh start".to_string(), false)),
            LeadEvent::TurnFinished { stop_reason, .. } if stop_reason == "error" => {
                Some(("A turn failed".to_string(), true))
            }
            _ => None,
        };
        if let Some((text, attention)) = line {
            out.push((r.ts, text, attention));
        }
    }
    out
}

fn shorten(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &str, group: GroupKind, last_active: u64) -> SideEntry {
        SideEntry {
            key: key.into(),
            href: format!("/agents/{key}"),
            name: key.to_uppercase(),
            icon: (key != "lead").then(|| AgentIcon {
                shape: "cube".into(),
                color: "teal".into(),
            }),
            group,
            status: (group == GroupKind::Working).then(|| "Working on it".into()),
            sub: None,
            working: group == GroupKind::Working,
            head: None,
            urls: MenuUrls::of(key, group == GroupKind::Pinned, false),
            last_active,
        }
    }

    fn side(entries: Vec<SideEntry>) -> SideData {
        SideData {
            entries,
            ..SideData::default()
        }
    }

    /// The keys of each shown group, in order.
    fn order(side: &SideData) -> Vec<(GroupKind, Vec<&str>)> {
        side_groups(side, None)
            .into_iter()
            .filter(|g| !g.agents.is_empty())
            .map(|g| (g.kind, g.agents.iter().map(|a| a.key).collect()))
            .collect()
    }

    #[test]
    fn working_agents_sit_last_and_idle_ones_most_recent_first() {
        let s = side(vec![
            entry("lead", GroupKind::Pinned, 0),
            entry("w1", GroupKind::Working, 900),
            entry("old", GroupKind::Idle, 100),
            entry("ask", GroupKind::Attention, 50),
            entry("new", GroupKind::Idle, 300),
            entry("w2", GroupKind::Working, 10),
            entry("mid", GroupKind::Idle, 200),
            entry("pin", GroupKind::Pinned, 1),
        ]);
        assert_eq!(
            order(&s),
            [
                (GroupKind::Attention, vec!["ask"]),
                (GroupKind::Pinned, vec!["lead", "pin"]),
                (GroupKind::Idle, vec!["new", "mid", "old"]),
                (GroupKind::Working, vec!["w1", "w2"]),
            ]
        );
    }

    #[test]
    fn an_agent_that_finishes_moves_back_up_into_the_idle_list() {
        let mut s = side(vec![
            entry("lead", GroupKind::Pinned, 0),
            entry("a", GroupKind::Idle, 100),
            entry("b", GroupKind::Working, 50),
        ]);
        assert_eq!(order(&s)[2], (GroupKind::Working, vec!["b"]));
        // b finishes: it was active just now, so it heads the idle list.
        s.entries[2] = entry("b", GroupKind::Idle, 200);
        assert_eq!(
            order(&s),
            [
                (GroupKind::Pinned, vec!["lead"]),
                (GroupKind::Idle, vec!["b", "a"]),
            ]
        );
        // Same time: by name, so the order doesn't flicker.
        s.entries[1].last_active = 200;
        assert_eq!(order(&s)[1], (GroupKind::Idle, vec!["a", "b"]));
    }
}
