use crate::model_client_bindings::ein::model_client::host::Host;
use crate::ModelClientHarnessState;
use ein_model_client::{HttpRequest, HttpResponse};

impl Host for ModelClientHarnessState {
    async fn log(&mut self, msg: String) {
        println!("[model client] {msg}");
    }

    async fn http_request(&mut self, request_json: String) -> Result<String, String> {
        let req: HttpRequest =
            serde_json::from_str(&request_json).map_err(|e| format!("invalid request: {e}"))?;

        let method = reqwest::Method::from_bytes(req.method.as_bytes())
            .map_err(|e| format!("invalid HTTP method: {e}"))?;

        let mut builder = self.http_client.request(method, &req.url);
        for (k, v) in &req.headers {
            builder = builder.header(k, v);
        }
        builder = builder.body(req.body);

        let response = builder.send().await.map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        let body = response.text().await.map_err(|e| e.to_string())?;

        serde_json::to_string(&HttpResponse { status, body }).map_err(|e| e.to_string())
    }
}
