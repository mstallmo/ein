use wasmtime::component::bindgen;

bindgen!({
    world: "plugin",
    path: "../../wit/plugin",
    imports: { default: async },
    exports: { default: async }
});
