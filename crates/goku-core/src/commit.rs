use crate::error::RecorderError;
use crate::imagebuf::{prepare_image, PersistableImage, RawImage};
use crate::types::{
    DiscardReason, FocusIdentity, ImageStatus, OnUnmappable, PixelPolicy, PixelRect, SecretHit,
};

#[derive(Debug, Clone)]
pub struct CaptureFacts {
    pub before: FocusIdentity,
    pub after: FocusIdentity,
    pub pixels_ok: bool,
    pub denied: bool,
    pub secure_input: bool,
    pub permission_missing: bool,
    pub text: String,
    pub hits: Vec<SecretHit>,
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FramePlan {
    Discard { reason: DiscardReason },
    Row {
        text: String,
        labels: Vec<String>,
        image: Option<PersistableImage>,
        image_status: ImageStatus,
    },
}

pub fn plan_frame(
    facts: &CaptureFacts,
    raw: Option<RawImage>,
    policy: PixelPolicy,
    forced: &[PixelRect],
) -> Result<FramePlan, RecorderError> {
    if !facts.before.same_window(&facts.after) {
        return Ok(FramePlan::Discard {
            reason: DiscardReason::FocusIncoherent,
        });
    }
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

fn row(facts: &CaptureFacts, image: Option<PersistableImage>, image_status: ImageStatus) -> FramePlan {
    FramePlan::Row {
        text: facts.text.clone(),
        labels: facts.labels.clone(),
        image,
        image_status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imagebuf::RawImage;

    fn ident(window: u64) -> FocusIdentity {
        FocusIdentity {
            pid: 7,
            window_id: window,
            app_id: "app".into(),
        }
    }

    fn facts(after: u64) -> CaptureFacts {
        CaptureFacts {
            before: ident(1),
            after: ident(after),
            pixels_ok: true,
            denied: false,
            secure_input: false,
            permission_missing: false,
            text: "hello".into(),
            hits: vec![],
            labels: vec![],
        }
    }

    fn px() -> RawImage {
        RawImage::new(1, 1, vec![8, 8, 8, 255]).unwrap()
    }

    #[test]
    fn focus_change_discards_pixels() {
        let plan = plan_frame(
            &facts(2),
            Some(px()),
            PixelPolicy::SecretRegions {
                on_unmappable: OnUnmappable::WithholdImage,
            },
            &[],
        )
        .unwrap();
        assert!(matches!(plan, FramePlan::Discard { .. }));
    }

    #[test]
    fn unmapped_secret_withholds_and_keeps_redacted_text() {
        let mut facts = facts(1);
        facts.text = "key [REDACTED:OPENAI_KEY]".into();
        facts.hits = vec![SecretHit {
            label: "OPENAI_KEY".into(),
            start: 4,
            end: 10,
            region: None,
        }];
        facts.labels = vec!["OPENAI_KEY".into()];
        let plan = plan_frame(
            &facts,
            Some(px()),
            PixelPolicy::SecretRegions {
                on_unmappable: OnUnmappable::WithholdImage,
            },
            &[],
        )
        .unwrap();
        match plan {
            FramePlan::Row {
                image,
                image_status,
                text,
                ..
            } => {
                assert!(image.is_none());
                assert_eq!(image_status, ImageStatus::WithheldRedaction);
                assert!(!text.contains("sk-"));
            }
            FramePlan::Discard { .. } => panic!("should store a withheld row"),
        }
    }

    #[test]
    fn ready_row_carries_a_redacted_image() {
        let plan = plan_frame(
            &facts(1),
            Some(px()),
            PixelPolicy::SecretRegions {
                on_unmappable: OnUnmappable::WithholdImage,
            },
            &[],
        )
        .unwrap();
        match plan {
            FramePlan::Row {
                image: Some(img),
                image_status,
                ..
            } => {
                assert_eq!(image_status, ImageStatus::Ready);
                assert!(matches!(img, PersistableImage::Redacted(_)));
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
