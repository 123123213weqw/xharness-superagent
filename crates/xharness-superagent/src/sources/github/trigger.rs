//! Normalized GitHub events use the pinned product's stable identities.
use super::{repo_metadata, Error, Result};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use xharness_superagent_core::{
    ids::key,
    model::{ActorRef, Addressed, ChannelRef, ThreadRef, Trigger},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct User {
    pub login: String,
    #[serde(
        rename = "type",
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_string"
    )]
    pub kind: Option<String>,
}
/// Optional but non-null REST field. Keep malformed account metadata out of
/// intake rather than accidentally interpreting a null type as a human User.
pub(crate) fn optional_string<'de, D: Deserializer<'de>>(
    de: D,
) -> std::result::Result<Option<String>, D::Error> {
    String::deserialize(de).map(Some)
}
fn user<'de, D: Deserializer<'de>>(de: D) -> std::result::Result<User, D::Error> {
    Ok(Option::<User>::deserialize(de)?.unwrap_or(User {
        login: "ghost".into(),
        kind: Some("User".into()),
    }))
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Comment {
    pub id: u64,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(deserialize_with = "user")]
    pub user: User,
    #[serde(default)]
    pub html_url: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub submitted_at: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub commit_id: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub line: Option<u64>,
    #[serde(default)]
    pub original_line: Option<u64>,
    #[serde(default)]
    pub diff_hunk: Option<String>,
    #[serde(default)]
    pub in_reply_to_id: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Issue {
    #[serde(flatten)]
    pub comment: Comment,
    pub number: u64,
    pub title: String,
    pub state: String,
    #[serde(default)]
    pub pull_request: Option<PullRequestUrl>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PullRequestUrl {
    pub url: String,
}
impl Issue {
    pub fn validate(&self) -> Result<()> {
        if self.comment.created_at.is_none() || self.comment.html_url.is_none() {
            return Err(Error::Protocol(
                "GitHub Issue has no creation time or URL".into(),
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Conversation,
    Inline,
    Review,
}
impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Conversation => "conversation",
            Self::Inline => "inline",
            Self::Review => "review",
        }
    }
}
#[derive(Clone, Debug)]
pub struct PullRef {
    pub number: u64,
    pub is_pr: bool,
    pub url: Option<String>,
    pub pr_author: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Address {
    pub number: u64,
    pub pr: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discussion: Option<u64>,
}

pub fn address(url: &str, repository: &str) -> Option<Address> {
    let lower = url.to_ascii_lowercase();
    let rest = if lower.starts_with("https://api.github.com/repos/") {
        url.get(29..)?
    } else if lower.starts_with("https://github.com/") {
        url.get(19..)?
    } else {
        return None;
    };
    let mut parts = rest.splitn(4, '/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    if !format!("{owner}/{repo}").eq_ignore_ascii_case(repository) {
        return None;
    }
    let kind = parts.next()?.to_ascii_lowercase();
    if !["issues", "pull", "pulls"].contains(&kind.as_str()) {
        return None;
    }
    let last = parts.next()?;
    let (number, fragment) = last
        .split_once('#')
        .map(|(a, b)| (a, Some(b)))
        .unwrap_or((last, None));
    let number = number.strip_suffix('/').unwrap_or(number);
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let discussion = if let Some(fragment) = fragment {
        if fragment
            .get(..12)
            .is_some_and(|p| p.eq_ignore_ascii_case("discussion_r"))
            && fragment
                .get(12..)
                .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
        {
            Some(fragment.get(12..)?.parse().ok()?)
        } else {
            if !fragment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            {
                return None;
            }
            None
        }
    } else {
        None
    };
    Some(Address {
        number: number.parse().ok()?,
        pr: kind != "issues",
        discussion,
    })
}
pub fn channel(number: u64, url: Option<&str>) -> ChannelRef {
    let mut address = BTreeMap::from([("number".into(), number.to_string())]);
    if let Some(url) = url {
        address.insert("url".into(), url.into());
    }
    ChannelRef {
        source: "github".into(),
        thread: format!("github:{number}"),
        address,
    }
}
pub fn is_human(user: &User) -> bool {
    !user.login.ends_with("[bot]")
        && user
            .kind
            .as_deref()
            .is_none_or(|kind| kind.is_empty() || kind == "User")
}
pub fn mentions(text: &str, login: &str) -> bool {
    if login.is_empty() {
        return false;
    }
    text.match_indices('@').any(|(at, _)| {
        let before = text[..at].chars().next_back();
        if before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '@') {
            return false;
        }
        let Some(tail) = text.get(at + 1..) else {
            return false;
        };
        let Some(candidate) = tail.get(..login.len()) else {
            return false;
        };
        candidate.eq_ignore_ascii_case(login)
            && tail[login.len()..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
    })
}
pub fn review_place(comment: &Comment) -> String {
    let Some(path) = &comment.path else {
        return String::new();
    };
    let mut place = path.clone();
    if let Some(line) = comment.line.or(comment.original_line) {
        place.push_str(&format!(":{line}"));
    }
    let lines: Vec<_> = comment
        .diff_hunk
        .as_deref()
        .unwrap_or("")
        .split('\n')
        .collect();
    let hunk = lines[lines.len().saturating_sub(3)..].join("\n");
    if !hunk.is_empty() {
        place.push('\n');
        place.push_str(&hunk);
    }
    place
}
pub fn comment_trigger(
    comment: &Comment,
    reference: &PullRef,
    kind: Kind,
    agent: &str,
    now: &str,
) -> Result<Trigger> {
    let number = reference.number.to_string();
    let fallback = format!("github:{}", reference.number);
    let id = key(&[
        "github-comment",
        comment.html_url.as_deref().unwrap_or(&fallback),
        &comment.id.to_string(),
        comment
            .updated_at
            .as_deref()
            .or(comment.submitted_at.as_deref())
            .unwrap_or(""),
    ])?;
    let mut text = Vec::new();
    if kind == Kind::Review {
        if let Some(state) = comment
            .state
            .as_deref()
            .filter(|s| !s.is_empty() && *s != "COMMENTED")
        {
            text.push(format!(
                "{state} review of commit {}",
                comment.commit_id.as_deref().unwrap_or("unknown")
            ));
        }
    }
    if kind == Kind::Inline {
        let place = review_place(comment);
        if !place.is_empty() {
            text.push(place);
        }
    }
    if let Some(body) = comment.body.as_ref().filter(|s| !s.is_empty()) {
        text.push(body.clone());
    }
    let mut meta = repo_metadata(reference.number, reference.is_pr);
    meta.insert("commentId".into(), json!(comment.id));
    meta.insert("kind".into(), json!(kind));
    if let Some(author) = reference.pr_author.as_ref().filter(|a| !a.is_empty()) {
        meta.insert("prAuthor".into(), json!(author));
    }
    if let Some(path) = comment.path.as_ref().filter(|s| !s.is_empty()) {
        meta.insert("path".into(), json!(path));
        meta.insert(
            "line".into(),
            comment
                .line
                .or(comment.original_line)
                .map_or(Value::Null, |line| json!(line)),
        );
    }
    if let Some(id) = comment.in_reply_to_id.filter(|id| *id != 0) {
        meta.insert("inReplyTo".into(), json!(id));
    }
    let url = comment
        .html_url
        .as_ref()
        .or(reference.url.as_ref())
        .filter(|u| !u.is_empty())
        .cloned();
    Ok(Trigger {
        id,
        source: "github".into(),
        threads: vec![ThreadRef {
            source: "github".into(),
            id: number,
        }],
        actor: ActorRef {
            source: "github".into(),
            id: comment.user.login.clone(),
        },
        human: is_human(&comment.user) && !comment.user.login.eq_ignore_ascii_case(agent),
        addressed: mentions(comment.body.as_deref().unwrap_or(""), agent)
            .then_some(Addressed::Mention),
        at: comment
            .updated_at
            .as_deref()
            .or(comment.submitted_at.as_deref())
            .or(comment.created_at.as_deref())
            .unwrap_or(now)
            .into(),
        text: text.join("\n\n"),
        channel: Some(channel(reference.number, comment.html_url.as_deref())),
        url,
        meta,
    })
}
pub fn issue_trigger(issue: &Issue, agent: &str) -> Result<Trigger> {
    issue.validate()?;
    let comment = &issue.comment;
    let is_pr = issue.pull_request.is_some();
    let mut meta = repo_metadata(issue.number, is_pr);
    meta.insert("kind".into(), json!(if is_pr { "pull" } else { "issue" }));
    if is_pr {
        meta.insert("prAuthor".into(), json!(comment.user.login));
    }
    let at = comment
        .created_at
        .clone()
        .ok_or_else(|| Error::Protocol("Issue creation time missing".into()))?;
    let url = comment
        .html_url
        .clone()
        .ok_or_else(|| Error::Protocol("Issue URL missing".into()))?;
    Ok(Trigger {
        id: key(&["github-issue", &issue.number.to_string()])?,
        source: "github".into(),
        threads: vec![ThreadRef {
            source: "github".into(),
            id: issue.number.to_string(),
        }],
        actor: ActorRef {
            source: "github".into(),
            id: comment.user.login.clone(),
        },
        human: is_human(&comment.user) && !comment.user.login.eq_ignore_ascii_case(agent),
        addressed: mentions(comment.body.as_deref().unwrap_or(""), agent)
            .then_some(Addressed::Mention),
        at,
        text: format!(
            "{}\n\n{}",
            issue.title,
            comment.body.as_deref().unwrap_or("")
        ),
        channel: Some(channel(issue.number, Some(&url))),
        url: Some(url),
        meta,
    })
}
pub enum CiTarget<'a> {
    Pull {
        number: u64,
        url: &'a str,
        author: &'a str,
    },
    Branch {
        name: &'a str,
        sha: &'a str,
        url: &'a str,
    },
}
pub fn ci_trigger(
    target: CiTarget<'_>,
    revision: u64,
    summary: String,
    now: &str,
) -> Result<Trigger> {
    let (number, branch, sha, url, author) = match target {
        CiTarget::Pull {
            number,
            url,
            author,
        } => (Some(number), None, "", url, Some(author)),
        CiTarget::Branch { name, sha, url } => (None, Some(name), sha, url, None),
    };
    let identity = number
        .map(|n| n.to_string())
        .unwrap_or_else(|| format!("branch:{}", branch.unwrap_or("")));
    let mut meta = number.map(|n| repo_metadata(n, true)).unwrap_or_default();
    meta.insert("ci".into(), json!(true));
    meta.insert("revision".into(), json!(revision));
    if let Some(branch) = branch {
        meta.insert("branch".into(), json!(branch));
        meta.insert("sha".into(), json!(sha));
    }
    if let Some(author) = author {
        meta.insert("prAuthor".into(), json!(author));
    }
    Ok(Trigger {
        id: key(&["github-ci", &identity, &revision.to_string()])?,
        source: "github".into(),
        threads: vec![ThreadRef {
            source: "github".into(),
            id: number
                .map(|n| n.to_string())
                .unwrap_or_else(|| format!("ci:{}", branch.unwrap_or(""))),
        }],
        actor: ActorRef {
            source: "github".into(),
            id: String::new(),
        },
        human: false,
        addressed: None,
        at: now.into(),
        text: summary,
        channel: number.map(|n| channel(n, None)),
        url: Some(url.into()),
        meta,
    })
}
