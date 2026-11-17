use goku_core::RecorderEvent;
use goku_core::{
    CaptureFilter, Config, FrameId, PermissionKind, RecentFramesQuery, Recorder, TextMode,
};
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;
use std::sync::{mpsc::Receiver, Mutex};

fn error(err: impl std::fmt::Display) -> napi::Error {
    napi::Error::from_reason(err.to_string())
}

fn kind(value: &str) -> napi::Result<PermissionKind> {
    match value {
        "screen" => Ok(PermissionKind::Screen),
        "accessibility" => Ok(PermissionKind::Accessibility),
        "input" => Ok(PermissionKind::Input),
        _ => Err(error("unknown permission kind")),
    }
}

#[napi]
pub struct NativeRecorder {
    recorder: Recorder,
    events: Mutex<Receiver<RecorderEvent>>,
}

#[napi]
impl NativeRecorder {
    #[napi(constructor)]
    pub fn new(data_dir: String, filter_json: Option<String>) -> napi::Result<Self> {
        let mut config = Config::new(data_dir).map_err(error)?;
        config.text_mode = TextMode::Both;
        if let Some(json) = filter_json {
            let mut filter: CaptureFilter = serde_json::from_str(&json).map_err(error)?;
            filter
                .exclude_apps
                .extend(CaptureFilter::with_default_excludes().exclude_apps);
            config.filter = filter;
        }
        let recorder = Recorder::new(config).map_err(error)?;
        let events = Mutex::new(recorder.subscribe());
        Ok(Self { recorder, events })
    }

    #[napi]
    pub fn permissions(&self) -> napi::Result<String> {
        serde_json::to_string(&self.recorder.permissions().map_err(error)?).map_err(error)
    }

    #[napi]
    pub fn request_permission(&self, value: String) -> napi::Result<()> {
        self.recorder
            .request_permission(kind(&value)?)
            .map_err(error)
    }

    #[napi]
    pub fn open_settings(&self, value: String) -> napi::Result<()> {
        self.recorder.open_settings(kind(&value)?).map_err(error)
    }

    #[napi]
    pub fn start(&self) -> napi::Result<()> {
        if !self.recorder.permissions().map_err(error)?.can_record {
            return Err(error(
                "Grant Screen Recording, then quit and reopen Goku Capture Test",
            ));
        }
        self.recorder.start().map_err(error)
    }

    #[napi]
    pub fn stop(&self) -> napi::Result<()> {
        self.recorder.stop().map_err(error)
    }

    #[napi]
    pub fn pause(&self) -> napi::Result<()> {
        self.recorder.pause().map_err(error)
    }

    #[napi]
    pub fn resume(&self) -> napi::Result<()> {
        self.recorder.resume().map_err(error)
    }

    #[napi]
    pub fn capture_now(&self) -> napi::Result<f64> {
        self.recorder
            .capture_now()
            .map(|id| id.0 as f64)
            .map_err(error)
    }

    #[napi]
    pub fn status(&self) -> napi::Result<String> {
        serde_json::to_string(&self.recorder.status().map_err(error)?).map_err(error)
    }

    #[napi]
    pub fn events(&self) -> napi::Result<String> {
        let receiver = self
            .events
            .lock()
            .map_err(|_| error("event receiver unavailable"))?;
        serde_json::to_string(&receiver.try_iter().collect::<Vec<_>>()).map_err(error)
    }

    #[napi]
    pub fn recent_frames(&self) -> napi::Result<String> {
        serde_json::to_string(
            &self
                .recorder
                .recent_frames(RecentFramesQuery::default())
                .map_err(error)?,
        )
        .map_err(error)
    }

    #[napi]
    pub fn frame(&self, id: i32) -> napi::Result<String> {
        serde_json::to_string(&self.recorder.frame(FrameId(i64::from(id))).map_err(error)?)
            .map_err(error)
    }

    #[napi]
    pub fn frame_image(&self, id: i32) -> napi::Result<Buffer> {
        self.recorder
            .frame_image(FrameId(i64::from(id)))
            .map(Buffer::from)
            .map_err(error)
    }
}
