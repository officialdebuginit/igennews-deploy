use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub service: &'static str,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct DependencyHealth {
    pub pooled_database: bool,
    pub direct_database: bool,
    pub object_storage: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ReadinessResponse {
    pub status: &'static str,
    pub dependencies: DependencyHealth,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct BuildInfo {
    pub service: &'static str,
    pub version: &'static str,
    pub git_sha: &'static str,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: &'static str,
    pub refresh_token: Option<String>,
    pub expires_in: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct RefreshRequest {
    pub refresh_token: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct IdentityUser {
    pub id: String,
    pub email: String,
    pub handle: String,
    pub display_name: String,
    pub role: String,
    pub department: Option<String>,
    pub is_admin: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ApiError {
    pub detail: String,
}

/// The 17 top-level IPTC **Media Topics** (`(qcode, label)`) — the standard news
/// subject taxonomy. Shared so the editor (WASM) can offer them and the feed
/// serializers (server) can expand stored qcodes into `subject` entries. Scheme:
/// `http://cv.iptc.org/newscodes/mediatopic/`.
pub const MEDIA_TOPICS: [(&str, &str); 17] = [
    ("01000000", "arts, culture, entertainment and media"),
    ("02000000", "crime, law and justice"),
    ("03000000", "disaster, accident and emergency incident"),
    ("04000000", "economy, business and finance"),
    ("05000000", "education"),
    ("06000000", "environment"),
    ("07000000", "health"),
    ("08000000", "human interest"),
    ("09000000", "labour"),
    ("10000000", "lifestyle and leisure"),
    ("11000000", "politics"),
    ("12000000", "religion"),
    ("13000000", "science and technology"),
    ("14000000", "society"),
    ("15000000", "sport"),
    ("16000000", "conflict, war and peace"),
    ("17000000", "weather"),
];

/// Resolves a Media Topic qcode to its label, if it is a known top-level topic.
#[must_use]
pub fn media_topic_label(qcode: &str) -> Option<&'static str> {
    MEDIA_TOPICS
        .iter()
        .find(|(code, _)| *code == qcode)
        .map(|(_, label)| *label)
}

/// IPTC **Digital Source Type** `NewsCodes` (`(code, label)`) — the AI-disclosure
/// vocabulary that declares how a piece of media was produced (human capture vs.
/// generative AI). Required by EU AI Act Art. 50 machine-readable marking and read by
/// Google. Scheme: `http://cv.iptc.org/newscodes/digitalsourcetype/` (`NewsML`-G2 alias
/// `digsrctype`). A curated newsroom-relevant subset of the full vocabulary.
pub const DIGITAL_SOURCE_TYPES: [(&str, &str); 9] = [
    ("digitalCapture", "Digital capture (camera)"),
    ("minorHumanEdits", "Human capture, minor edits"),
    ("compositeCapture", "Composite of captured elements"),
    ("algorithmicallyEnhanced", "Algorithmically enhanced"),
    ("dataDrivenMedia", "Data-driven visualisation"),
    ("digitalArt", "Digital art (human-made)"),
    ("compositeSynthetic", "Composite with synthetic (AI) elements"),
    ("trainedAlgorithmicMedia", "AI-generated (trained model)"),
    ("algorithmicMedia", "Purely algorithmic"),
];

/// The IPTC scheme URI Digital Source Type codes live under.
pub const DIGITAL_SOURCE_TYPE_SCHEME: &str = "http://cv.iptc.org/newscodes/digitalsourcetype/";

/// Resolves a Digital Source Type code to its label, if it is a known value.
#[must_use]
pub fn digital_source_type_label(code: &str) -> Option<&'static str> {
    DIGITAL_SOURCE_TYPES
        .iter()
        .find(|(key, _)| *key == code)
        .map(|(_, label)| *label)
}

/// One selectable webhook event: `(event type, human summary)`.
pub type WebhookEvent = (&'static str, &'static str);

/// A group of webhook events for the picker: `(label, resource, events)`, where
/// `resource` yields the group's `resource.*` wildcard.
pub type WebhookEventGroup = (&'static str, &'static str, &'static [WebhookEvent]);

/// The catalogue of outbound webhook event types a subscription can select, grouped
/// by resource for the endpoint UI. Each group is `(label, resource, events)` where
/// `resource` yields the `resource.*` wildcard and every event is `(type, summary)`.
///
/// This is the **complete** event surface, derived by auditing every mutation in the
/// newsroom service (each `crate::audit(...)` action), so the picker can offer every
/// possible event. The dispatcher matches these against endpoint patterns (see
/// `webhooks::event_matches`). Emission is being rolled out — some are already on the
/// bus and the rest are wired as the corresponding mutations gain their
/// `enqueue_event` call — so a subscription may select any of them, forward-compatibly,
/// and simply receives nothing until that event is emitted. Credential/security
/// actions (admin password resets, recovery-email sends) are deliberately **excluded**
/// from the outbound surface and never appear here.
pub const WEBHOOK_EVENT_GROUPS: &[WebhookEventGroup] = &[
    // -- Editorial ----------------------------------------------------------------
    (
        "Articles & stories",
        "article",
        &[
            ("article.created", "A story was created"),
            ("article.updated", "A story's content or metadata changed"),
            ("article.submitted", "A story was submitted for review"),
            ("article.stage_changed", "A story moved to a new workflow stage"),
            ("article.approved", "A story cleared editorial review"),
            ("article.rejected", "A story was sent back in review"),
            ("article.commissioned", "A pitch was commissioned into a story"),
            ("article.scheduled", "A story was scheduled for release"),
            ("article.unscheduled", "A scheduled release was withdrawn"),
            ("article.rescheduled", "A story's release time changed"),
            ("article.published", "A story went live"),
            ("article.published.edited", "A live story was edited"),
            ("article.unpublished", "A story was taken offline"),
            ("article.embargo_set", "An embargo was placed on a story"),
            ("article.embargo_lifted", "A story's embargo was lifted"),
            ("article.expired", "A story reached its expiry"),
            ("article.corrected", "A correction was applied to a story"),
            ("article.retracted", "A story was retracted"),
            ("article.deleted", "A story was deleted"),
        ],
    ),
    (
        "Story versions",
        "version",
        &[("version.created", "A new story version was cut")],
    ),
    (
        "Releases",
        "release",
        &[
            ("release.scheduled", "A release was scheduled"),
            ("release.channel_scheduled", "A per-channel release was scheduled"),
            ("release.published", "A release went out on all channels"),
            ("release.channel_published", "A single channel published"),
            ("release.rescheduled", "A release slot moved"),
            ("release.cancelled", "A release was cancelled"),
            ("release.expired", "A release reached its expiry"),
            ("release.failed", "A release attempt failed the gate"),
        ],
    ),
    (
        "Corrections",
        "correction",
        &[
            ("correction.created", "A correction was filed"),
            ("correction.resolved", "A correction was resolved"),
            ("correction.retraction", "A retraction was issued"),
        ],
    ),
    (
        "Pitches",
        "pitch",
        &[
            ("pitch.created", "A pitch was submitted"),
            ("pitch.commissioned", "A pitch was commissioned"),
            ("pitch.declined", "A pitch was declined"),
        ],
    ),
    (
        "Tasks",
        "task",
        &[
            ("task.created", "A task was created"),
            ("task.assigned", "A task was assigned"),
            ("task.updated", "A task changed"),
            ("task.completed", "A task was completed"),
            ("task.deleted", "A task was removed"),
        ],
    ),
    (
        "Coverage & planning",
        "coverage_event",
        &[
            ("coverage_event.created", "A coverage/planning event was added"),
            ("coverage_event.updated", "A coverage event changed"),
            ("coverage_event.deleted", "A coverage event was removed"),
        ],
    ),
    (
        "Reviews",
        "review",
        &[
            ("review.requested", "A review was requested"),
            ("review.approved", "A review was approved"),
            ("review.rejected", "A review was rejected"),
        ],
    ),
    (
        "Comments",
        "comment",
        &[
            ("comment.created", "A comment was posted"),
            ("comment.resolved", "A comment thread was resolved"),
            ("comment.deleted", "A comment was deleted"),
        ],
    ),
    (
        "Assets & media",
        "asset",
        &[
            ("asset.uploaded", "A media asset was uploaded"),
            ("asset.created", "An asset record was created"),
            ("asset.updated", "An asset's metadata changed"),
            ("asset.renamed", "An asset was renamed"),
            ("asset.moved", "An asset moved in the library"),
            ("asset.deleted", "An asset was deleted"),
            ("asset.verification_changed", "An asset's verification status changed"),
        ],
    ),
    (
        "Folders",
        "folder",
        &[
            ("folder.created", "A Drive folder was created"),
            ("folder.renamed", "A folder was renamed"),
            ("folder.deleted", "A folder was deleted"),
        ],
    ),
    (
        "Distribution channels",
        "channel",
        &[("channel.saved", "A distribution channel was configured")],
    ),
    (
        "Front page",
        "frontpage",
        &[("frontpage.slot_set", "A front-page slot was set")],
    ),
    // -- Fact-checking ------------------------------------------------------------
    (
        "Sources",
        "source",
        &[
            ("source.created", "A source was added"),
            ("source.approved", "A source was approved"),
            ("source.access_granted", "Access to a protected source was granted"),
        ],
    ),
    (
        "Evidence",
        "evidence",
        &[
            ("evidence.created", "A piece of evidence was filed"),
            ("evidence.status_changed", "An evidence item's status changed"),
        ],
    ),
    (
        "Claims",
        "claim",
        &[
            ("claim.created", "A claim was logged for checking"),
            ("claim.approved", "A claim was verified"),
            ("claim.rejected", "A claim was rejected"),
        ],
    ),
    // -- Legal & commerce ---------------------------------------------------------
    (
        "Legal documents",
        "legal",
        &[
            ("legal.created", "A legal document was created"),
            ("legal.party_added", "A party was added to a document"),
            ("legal.party_removed", "A party was removed from a document"),
            ("legal.sign_requested", "A signature was requested"),
            ("legal.party_signed", "A party signed a document"),
            ("legal.executed", "A document was fully executed"),
            ("legal.declined", "A signature was declined"),
            ("legal.voided", "A document was voided"),
            ("legal.hold", "A legal hold was placed"),
            ("legal.edited", "A document was edited"),
            ("legal.deleted", "A document was deleted"),
        ],
    ),
    (
        "Subscriptions",
        "subscription",
        &[
            ("subscription.started", "A reader subscription started"),
            ("subscription.renewed", "A subscription renewed"),
            ("subscription.canceled", "A subscription was cancelled"),
        ],
    ),
    // -- Organisation -------------------------------------------------------------
    (
        "Desks",
        "desk",
        &[
            ("desk.created", "A desk was created"),
            ("desk.updated", "A desk changed"),
            ("desk.archived", "A desk was archived"),
            ("desk.unarchived", "A desk was un-archived"),
            ("desk.deleted", "A desk was deleted"),
            ("desk.reparented", "A desk moved in the hierarchy"),
            ("desk.member_added", "A member was added to a desk"),
            ("desk.member_removed", "A member was removed from a desk"),
            ("desk.slas_updated", "A desk's SLAs were changed"),
            ("desk.schedule_updated", "A desk's on-call schedule changed"),
            ("desk.smtp_updated", "A desk's outbound SMTP was configured"),
            ("desk.invitation_created", "A desk invitation was sent"),
            ("desk.invitation_accepted", "A desk invitation was accepted"),
            ("desk.invitation_declined", "A desk invitation was declined"),
        ],
    ),
    (
        "Sector applications",
        "application",
        &[
            ("application.created", "A sector-membership application was filed"),
            ("application.approved", "A sector application was approved"),
            ("application.rejected", "A sector application was rejected"),
        ],
    ),
    (
        "Sub-sectors",
        "subsector",
        &[
            ("subsector.created", "An industry was added to a sector"),
            ("subsector.updated", "An industry was changed"),
            ("subsector.archived", "An industry was archived"),
            ("subsector.deleted", "An industry was removed"),
        ],
    ),
    (
        "Sector policy",
        "sector",
        &[("sector.policy.updated", "A sector's policy was updated")],
    ),
    (
        "People & profiles",
        "user",
        &[
            ("user.created", "A user account was created"),
            ("user.updated", "A user account was changed by an admin"),
            ("user.profile_updated", "A user updated their own profile"),
        ],
    ),
    // -- Governance (admin) -------------------------------------------------------
    (
        "Roles",
        "role",
        &[
            ("role.created", "A role was created"),
            ("role.updated", "A role's capabilities changed"),
            ("role.deleted", "A role was deleted"),
            ("role.assigned", "A role was granted"),
            ("role.unassigned", "A role was revoked"),
        ],
    ),
    (
        "Permissions",
        "permission",
        &[
            ("permission.upserted", "A capability grant was set"),
            ("permission.deleted", "A capability grant was removed"),
        ],
    ),
    (
        "Delegations",
        "delegation",
        &[
            ("delegation.created", "A delegation was granted"),
            ("delegation.revoked", "A delegation was revoked"),
        ],
    ),
    (
        "Feature flags",
        "flag",
        &[
            ("flag.created", "A feature flag was created"),
            ("flag.updated", "A feature flag changed"),
            ("flag.deleted", "A feature flag was deleted"),
            ("flag.emergency_disabled", "A feature flag was emergency-disabled"),
        ],
    ),
    // -- Community & workspace ----------------------------------------------------
    (
        "Community feed",
        "feed",
        &[
            ("feed.post_created", "A feed post was published"),
            ("feed.post_deleted", "A feed post was deleted"),
            ("feed.reply_created", "A reply was posted"),
            ("feed.reply_deleted", "A reply was deleted"),
            ("feed.liked", "A feed post was liked"),
            ("feed.reposted", "A feed post was reposted"),
        ],
    ),
    (
        "Attention queue",
        "attention",
        &[
            ("attention.assigned", "An item was routed to someone's queue"),
            ("attention.escalated", "An item was escalated"),
        ],
    ),
    (
        "Branding",
        "branding",
        &[("branding.updated", "Workspace branding changed")],
    ),
];

/// Every concrete event type in [`WEBHOOK_EVENT_GROUPS`], flattened.
#[must_use]
pub fn webhook_event_types() -> Vec<&'static str> {
    WEBHOOK_EVENT_GROUPS
        .iter()
        .flat_map(|(_, _, events)| events.iter().map(|(ty, _)| *ty))
        .collect()
}

#[cfg(test)]
mod media_topics_tests {
    use super::{MEDIA_TOPICS, media_topic_label};

    #[test]
    fn taxonomy_is_well_formed_and_resolvable() {
        // All 17 top-level topics present, each an 8-digit qcode, no duplicates.
        assert_eq!(MEDIA_TOPICS.len(), 17);
        let mut seen = std::collections::HashSet::new();
        for (qcode, label) in MEDIA_TOPICS {
            assert_eq!(qcode.len(), 8, "qcode {qcode} is not 8 digits");
            assert!(qcode.chars().all(|c| c.is_ascii_digit()), "qcode {qcode} not numeric");
            assert!(!label.trim().is_empty(), "empty label for {qcode}");
            assert!(seen.insert(qcode), "duplicate qcode {qcode}");
            // Round-trips through the resolver.
            assert_eq!(media_topic_label(qcode), Some(label));
        }
        // Unknown qcodes resolve to None rather than a wrong label.
        assert_eq!(media_topic_label("99999999"), None);
        assert_eq!(media_topic_label(""), None);
    }
}

#[cfg(test)]
mod webhook_event_tests {
    use super::{WEBHOOK_EVENT_GROUPS, webhook_event_types};

    #[test]
    fn catalogue_is_well_formed() {
        let mut seen = std::collections::HashSet::new();
        for (label, resource, events) in WEBHOOK_EVENT_GROUPS {
            assert!(!label.trim().is_empty(), "empty group label");
            assert!(!resource.is_empty(), "empty resource for {label}");
            assert!(!events.is_empty(), "group {label} has no events");
            for (ty, desc) in *events {
                // The picker's `resource.*` wildcard only subsumes events that share
                // the group's prefix — so every event must carry it.
                assert!(
                    ty.starts_with(&format!("{resource}.")),
                    "event {ty} does not start with its group resource {resource}."
                );
                assert!(!desc.trim().is_empty(), "empty description for {ty}");
                assert!(seen.insert(*ty), "duplicate event type {ty}");
            }
        }
        // A representative slice of the fabric is present.
        let all = webhook_event_types();
        for expected in [
            "article.published",
            "release.failed",
            "correction.created",
            "subscription.started",
            "subsector.created",
            "sector.policy.updated",
        ] {
            assert!(all.contains(&expected), "{expected} missing from the catalogue");
        }
    }
}
