use goku_core::{
    plan_frame, prepare_image, CaptureFacts, FocusIdentity, FramePlan, ImageStatus, OnUnmappable,
    PixelPolicy, PixelRect, RawImage, RecorderError, SecretHit,
};

/// A ready image is forbidden when focus changed, and when a secret has no
/// usable pixel rect under withhold. Anything else may be ready.
fn ready_is_forbidden(focus_changed: bool, hit: Hit, policy: PixelPolicy) -> bool {
    if focus_changed {
        return true;
    }
    hit.is_unmapped()
        && matches!(
            policy,
            PixelPolicy::SecretRegions {
                on_unmappable: OnUnmappable::WithholdImage,
            }
        )
}

#[derive(Clone, Copy)]
enum Hit {
    Absent,
    Mapped,
    Unmapped,
    EmptyRect,
}

impl Hit {
    fn is_unmapped(self) -> bool {
        matches!(self, Self::Unmapped | Self::EmptyRect)
    }

    fn name(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Mapped => "mapped",
            Self::Unmapped => "unmapped",
            Self::EmptyRect => "empty rect",
        }
    }

    fn hits(self) -> Vec<SecretHit> {
        let region = match self {
            Self::Absent => return Vec::new(),
            Self::Mapped => Some(PixelRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            }),
            Self::Unmapped => None,
            Self::EmptyRect => Some(PixelRect {
                x: 0,
                y: 0,
                width: 0,
                height: 1,
            }),
        };
        vec![SecretHit {
            label: "OPENAI_KEY".into(),
            start: 0,
            end: 4,
            region,
        }]
    }
}

fn policies() -> [PixelPolicy; 3] {
    [
        PixelPolicy::Off,
        PixelPolicy::SecretRegions {
            on_unmappable: OnUnmappable::WithholdImage,
        },
        PixelPolicy::SecretRegions {
            on_unmappable: OnUnmappable::RedactWholeImage,
        },
    ]
}

fn policy_name(policy: PixelPolicy) -> &'static str {
    match policy {
        PixelPolicy::Off => "off",
        PixelPolicy::SecretRegions {
            on_unmappable: OnUnmappable::WithholdImage,
        } => "withhold",
        PixelPolicy::SecretRegions {
            on_unmappable: OnUnmappable::RedactWholeImage,
        } => "whole-image",
    }
}

fn identity(window_id: u64) -> FocusIdentity {
    FocusIdentity {
        pid: 7,
        window_id,
        app_id: "com.example.App".into(),
    }
}

fn pixel() -> RawImage {
    RawImage::new(1, 1, vec![8, 8, 8, 255]).expect("1x1 rgba")
}

fn facts_for(focus_changed: bool, pixels_ok: bool, denied: bool, secure_input: bool, permission_missing: bool, hit: Hit) -> CaptureFacts {
    CaptureFacts {
        before: identity(1),
        after: identity(if focus_changed { 2 } else { 1 }),
        pixels_ok,
        denied,
        secure_input,
        permission_missing,
        text: "hello".into(),
        hits: hit.hits(),
        labels: vec![],
    }
}

#[derive(Debug)]
struct ReadyViolation {
    focus_changed: bool,
    pixels_ok: bool,
    denied: bool,
    secure_input: bool,
    permission_missing: bool,
    hit: &'static str,
    policy: &'static str,
    raw_present: bool,
    image_attached: bool,
}

impl std::fmt::Display for ReadyViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}, {}, {}, {}, {}, hit {}, policy {}, {} -> ImageStatus::Ready {}",
            if self.focus_changed {
                "focus changed"
            } else {
                "focus unchanged"
            },
            if self.pixels_ok {
                "pixels ok"
            } else {
                "pixels not ok"
            },
            if self.denied { "denied" } else { "not denied" },
            if self.secure_input {
                "secure input"
            } else {
                "not secure input"
            },
            if self.permission_missing {
                "permission missing"
            } else {
                "permission present"
            },
            self.hit,
            self.policy,
            if self.raw_present {
                "raw present"
            } else {
                "raw absent"
            },
            if self.image_attached {
                "with a persistable image"
            } else {
                "with no image"
            },
        )
    }
}

fn fixture_matches_focus_and_mapped() {
    let same = identity(1);
    let other = identity(2);
    assert!(
        same.same_window(&same),
        "same-window fixture is not same_window"
    );
    assert!(
        !same.same_window(&other),
        "changed-window fixture is not a focus change under same_window"
    );
    for hit in [Hit::Absent, Hit::Mapped, Hit::Unmapped, Hit::EmptyRect] {
        for secret in hit.hits() {
            assert_eq!(
                secret.mapped(),
                !hit.is_unmapped(),
                "hit axis {} does not match SecretHit::mapped",
                hit.name()
            );
        }
    }
}

/// Exhaustive check of the finite commit decision. Returns one entry for each
/// case where `decide` is willing to store `ImageStatus::Ready` in a forbidden case.
fn check_ready_image(
    decide: &dyn Fn(
        &CaptureFacts,
        Option<RawImage>,
        PixelPolicy,
        &[PixelRect],
    ) -> Result<FramePlan, RecorderError>,
) -> Vec<ReadyViolation> {
    fixture_matches_focus_and_mapped();
    let mut found = Vec::new();
    let mut cases = 0usize;
    for focus_changed in [false, true] {
        for pixels_ok in [false, true] {
            for denied in [false, true] {
                for secure_input in [false, true] {
                    for permission_missing in [false, true] {
                        for hit in [Hit::Absent, Hit::Mapped, Hit::Unmapped, Hit::EmptyRect] {
                            for policy in policies() {
                                for raw_present in [false, true] {
                                    cases += 1;
                                    let facts = facts_for(
                                        focus_changed,
                                        pixels_ok,
                                        denied,
                                        secure_input,
                                        permission_missing,
                                        hit,
                                    );
                                    let raw = if raw_present { Some(pixel()) } else { None };
                                    let Ok(FramePlan::Row {
                                        image_status: ImageStatus::Ready,
                                        image,
                                        ..
                                    }) = decide(&facts, raw, policy, &[])
                                    else {
                                        continue;
                                    };
                                    if !ready_is_forbidden(focus_changed, hit, policy) {
                                        continue;
                                    }
                                    found.push(ReadyViolation {
                                        focus_changed,
                                        pixels_ok,
                                        denied,
                                        secure_input,
                                        permission_missing,
                                        hit: hit.name(),
                                        policy: policy_name(policy),
                                        raw_present,
                                        image_attached: image.is_some(),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!(cases, 2 * 2 * 2 * 2 * 2 * 4 * 3 * 2, "dropped an axis");
    found
}

fn print_violations(label: &str, found: &[ReadyViolation]) {
    println!("{label}: checked 768 cases, ready violations: {}", found.len());
    for violation in found {
        println!("  {violation}");
    }
}

#[test]
fn ready_image_never_follows_a_focus_change_or_an_unmapped_secret_under_withhold() {
    let found = check_ready_image(&plan_frame);
    print_violations("plan_frame", &found);
    assert!(
        found.is_empty(),
        "plan_frame is willing to store a ready image in a forbidden case:\n{}",
        found
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn checker_reports_a_ready_image_when_the_focus_check_is_removed() {
    let found = check_ready_image(&plan_frame_without_focus_check);
    print_violations("focus check removed", &found);
    let wrong_window = found.iter().find(|v| {
        v.focus_changed
            && v.hit == "absent"
            && v.raw_present
            && v.pixels_ok
            && !v.denied
            && !v.secure_input
            && !v.permission_missing
            && v.image_attached
    });
    assert!(
        wrong_window.is_some(),
        "checker did not report a ready image from a focus change:\n{}",
        found
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

// Focus check omitted. This copy exists so the checker can fail without editing commit.rs.
fn plan_frame_without_focus_check(
    facts: &CaptureFacts,
    raw: Option<RawImage>,
    policy: PixelPolicy,
    forced: &[PixelRect],
) -> Result<FramePlan, RecorderError> {
    if facts.denied {
        return Ok(row(facts, None, ImageStatus::WithheldDenylist));
    }
    if facts.secure_input {
        return Ok(row(facts, None, ImageStatus::WithheldSecureInput));
    }
    if facts.permission_missing {
        return Ok(row(facts, None, ImageStatus::WithheldPermission));
    }
    if !facts.pixels_ok {
        return Ok(row(facts, None, ImageStatus::WithheldCapture));
    }
    let Some(raw) = raw else {
        return Ok(row(facts, None, ImageStatus::WithheldCapture));
    };
    match prepare_image(raw, &facts.hits, forced, policy) {
        Ok(image) => Ok(row(facts, Some(image), ImageStatus::Ready)),
        Err(_) => match policy {
            PixelPolicy::SecretRegions {
                on_unmappable: OnUnmappable::WithholdImage,
            } => Ok(row(facts, None, ImageStatus::WithheldRedaction)),
            other => Err(RecorderError::capture(format!(
                "prepare_image failed under {other:?}"
            ))),
        },
    }
}

fn row(facts: &CaptureFacts, image: Option<goku_core::PersistableImage>, image_status: ImageStatus) -> FramePlan {
    FramePlan::Row {
        text: facts.text.clone(),
        labels: facts.labels.clone(),
        image,
        image_status,
    }
}
