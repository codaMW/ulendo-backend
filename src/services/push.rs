/// Web Push service using manual VAPID signing via reqwest.
use crate::db::PushSubscription;

pub struct PushService {
    pub vapid_subject:     String,
    pub vapid_public_key:  String,
    pub vapid_private_key: String,
    client: reqwest::Client,
}

impl PushService {
    pub fn new(
        vapid_subject: String,
        vapid_public_key: String,
        vapid_private_key: String,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("failed to build reqwest client for push");
        Self { vapid_subject, vapid_public_key, vapid_private_key, client }
    }

    pub fn public_key(&self) -> &str { &self.vapid_public_key }

    pub async fn send(&self, sub: &PushSubscription, payload: String) -> anyhow::Result<()> {
        let token = self.build_vapid_jwt(&sub.endpoint)?;
        let resp = self.client
            .post(&sub.endpoint)
            .header("Authorization", format!("vapid t={token},k={}", self.vapid_public_key))
            .header("Content-Type", "application/json")
            .header("TTL", "86400")
            .body(payload)
            .send()
            .await?;
        if !resp.status().is_success() && resp.status().as_u16() != 201 {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("push failed {status}: {body}");
        }
        Ok(())
    }

    fn build_vapid_jwt(&self, endpoint: &str) -> anyhow::Result<String> {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        let url = reqwest::Url::parse(endpoint)?;
        let aud = format!("{}://{}", url.scheme(), url.host_str().unwrap_or(""));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?.as_secs();
        let header = URL_SAFE_NO_PAD.encode(r#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = URL_SAFE_NO_PAD.encode(
            serde_json::json!({"aud": aud, "exp": now + 43200, "sub": self.vapid_subject}).to_string()
        );
        let signing_input = format!("{header}.{claims}");
        let sig = self.es256_sign(signing_input.as_bytes())?;
        Ok(format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(&sig)))
    }

    fn es256_sign(&self, data: &[u8]) -> anyhow::Result<Vec<u8>> {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        use p256::{ecdsa::{signature::Signer, Signature, SigningKey}, SecretKey};
        let key_bytes = URL_SAFE_NO_PAD.decode(&self.vapid_private_key)?;
        let secret = SecretKey::from_slice(&key_bytes)?;
        let sig: Signature = SigningKey::from(secret).sign(data);
        Ok(sig.to_bytes().to_vec())
    }
}
