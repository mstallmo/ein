impl super::bindings::ein::host::host::Host for super::ModelClientState {
    async fn log(&mut self, msg: String) {
        println!("[model client] {msg}");
    }
}
