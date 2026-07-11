// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

impl super::bindings::ein::host::host::Host for super::ModelClientState {
    async fn log(&mut self, msg: String) {
        println!("[model client] {msg}");
    }
}
