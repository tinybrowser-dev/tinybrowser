use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

use crate::engine::Engine;
use crate::js::Stop;
use crate::{BrowserServices, FrameId, Mount, RemoteValue, RendererEvent, TabError};

pub struct EmbeddedRenderer {
    engine: Engine,
    stop: Arc<Stop>,
    ready_queued: bool,
}

impl EmbeddedRenderer {
    #[must_use]
    pub fn new(host: Arc<dyn BrowserServices>, identity: net::ValidIdentity) -> Self {
        Self::with_fonts(host, identity, &crate::Fonts::default())
    }

    #[must_use]
    pub fn with_fonts(
        host: Arc<dyn BrowserServices>,
        identity: net::ValidIdentity,
        fonts: &crate::Fonts,
    ) -> Self {
        let stop = Arc::new(Stop::new());
        let mut engine = Engine::with_fonts(
            host,
            Arc::clone(&stop),
            Arc::new(Notify::new()),
            identity,
            fonts,
        );
        engine.use_standalone_history();
        Self {
            engine,
            stop,
            ready_queued: false,
        }
    }

    pub fn mount(&mut self, mount: &Mount) -> Result<(), TabError> {
        self.engine.mount_frame(FrameId::MAIN, mount)?;
        let _yielded = self.drain_until(std::time::Instant::now() + crate::TURN_SCRIPT_BUDGET);
        Ok(())
    }

    pub fn mount_bounded(&mut self, mount: &Mount, budget: Duration) -> Result<(), TabError> {
        self.engine.mount_frame(FrameId::MAIN, mount)?;
        let _yielded = self.drain_until(std::time::Instant::now() + budget);
        Ok(())
    }

    pub fn eval(&mut self, source: &str) -> Result<String, TabError> {
        self.engine.eval_in(FrameId::MAIN, source)
    }

    pub fn execute(
        &mut self,
        source: &str,
        timeout: Option<Duration>,
    ) -> Result<RemoteValue, TabError> {
        self.engine
            .execute_remote_in(FrameId::MAIN, crate::ScriptSource::Page(source), timeout)
    }

    pub fn screenshot(&mut self, request: &crate::ScreenshotRequest) -> Result<Vec<u8>, TabError> {
        self.engine.screenshot_frame(FrameId::MAIN, request)
    }

    pub fn render(
        &mut self,
        request: &crate::ScreenshotRequest,
    ) -> Result<crate::RgbaImage, TabError> {
        self.engine.render_frame(FrameId::MAIN, request)
    }

    #[must_use]
    pub fn drain_ready(&mut self) -> bool {
        self.drain_until(std::time::Instant::now() + crate::TURN_SCRIPT_BUDGET)
    }

    #[must_use]
    pub fn drain_until(&mut self, deadline: std::time::Instant) -> bool {
        let yielded = self.engine.drain_ready_until(deadline);
        self.ready_queued = yielded;
        yielded
    }

    pub fn take_events(&mut self) -> Result<Vec<(FrameId, RendererEvent)>, TabError> {
        self.engine.take_events()
    }

    #[must_use]
    pub fn has_pending_dials(&self) -> bool {
        self.engine.has_pending_dials()
    }

    #[must_use]
    pub fn time_until_deadline(&self) -> Option<Duration> {
        if self.ready_queued || self.engine.has_ready_work() {
            return Some(Duration::ZERO);
        }
        self.engine
            .next_deadline()
            .map(|deadline| deadline.saturating_duration_since(tokio::time::Instant::now()))
    }

    #[cfg(test)]
    pub(crate) fn mount_for_test(&mut self, mount: &Mount) -> Result<(), TabError> {
        self.engine.mount_frame(FrameId::MAIN, mount)
    }

    #[cfg(test)]
    pub(crate) fn drain_for(&mut self, budget: Duration) -> bool {
        self.drain_until(std::time::Instant::now() + budget)
    }
}

impl Drop for EmbeddedRenderer {
    fn drop(&mut self) {
        self.stop.request();
        self.engine.shutdown();
    }
}
