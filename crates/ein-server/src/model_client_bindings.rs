use wasmtime::component::bindgen;

bindgen!({
    world: "model-client",
    path: "../../wit/model_client",
    imports: { default: async },
    exports: { default: async }
});
