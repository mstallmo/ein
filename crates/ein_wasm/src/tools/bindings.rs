// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

use wasmtime::component::bindgen;

bindgen!({
    world: "plugin",
    path: "../../wit/plugin",
    imports: { default: async },
    exports: { default: async }
});
