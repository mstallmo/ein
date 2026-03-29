use crate::ModelClientHarnessState;
use crate::model_client_bindings::ein::model_client::host::Host;

impl Host for ModelClientHarnessState {
    async fn log(&mut self, msg: String) {
        println!("[model client] {msg}");
    }
}
