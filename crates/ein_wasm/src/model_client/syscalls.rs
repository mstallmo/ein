// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

use ein_agent::AgentEvent;

impl super::bindings::ein::host::host::Host for super::ModelClientState {
    async fn log(&mut self, msg: String) {
        println!("[model client] {msg}");
    }
}

impl super::bindings::ein::model_client::streaming::Host for super::ModelClientState {
    async fn on_content_delta(&mut self, delta: String) {
        // Mark that this completion streamed, so the agent loop won't re-emit the
        // assembled text, then forward the chunk as a `ContentDelta`.
        self.content_streamed = true;
        if let Some(handler) = &self.event_handler {
            handler(AgentEvent::ContentDelta(delta)).await;
        }
    }

    async fn on_reasoning_delta(&mut self, delta: String) {
        // Reasoning is separate from the assistant text: it is never folded into
        // the final message, so it must not set `content_streamed`. Just forward
        // the chunk as a `ReasoningDelta`.
        if let Some(handler) = &self.event_handler {
            handler(AgentEvent::ReasoningDelta(delta)).await;
        }
    }
}
