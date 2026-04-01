use axum::{
    extract::{Multipart, State},
    http::StatusCode,
    Json,
};
use serde_json::{json, Value};
use crate::{AppError, AppResult, AppState};

pub async fn upload_photo(
    State(_state): State<AppState>,
    mut multipart: Multipart,
) -> AppResult<Json<Value>> {
    let mut file_bytes: Option<(Vec<u8>, String)> = None;

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        AppError::BadRequest(format!("multipart error: {}", e))
    })? {
        let name = field.name().unwrap_or("").to_string();
        if name == "file" {
            let content_type = field.content_type()
                .unwrap_or("image/jpeg").to_string();
            let data = field.bytes().await.map_err(|e| {
                AppError::BadRequest(format!("read error: {}", e))
            })?;
            file_bytes = Some((data.to_vec(), content_type));
        }
    }

    let (bytes, mime) = file_bytes.ok_or_else(|| {
        AppError::BadRequest("no file field in request".into())
    })?;

    if bytes.len() > 10 * 1024 * 1024 {
        return Err(AppError::BadRequest("file too large (max 10MB)".into()));
    }

    // Upload to imgur anonymously
    let client = reqwest::Client::new();
    let ext = if mime.contains("png") { "png" } else { "jpg" };
    let part = reqwest::multipart::Part::bytes(bytes.clone())
        .file_name(format!("ulendo.{}", ext))
        .mime_str(&mime)
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
    let form = reqwest::multipart::Form::new().part("image", part);

    let resp = client
        .post("https://api.imgur.com/3/image")
        .header("Authorization", "Client-ID 546c25a59c58ad7")
        .multipart(form)
        .send()
        .await;

    if let Ok(r) = resp {
        if r.status().is_success() {
            if let Ok(j) = r.json::<serde_json::Value>().await {
                if let Some(url) = j["data"]["link"].as_str() {
                    if url.starts_with("https://") {
                        return Ok(Json(json!({ "url": url, "provider": "imgur" })));
                    }
                }
            }
        }
    }

    // Fallback: nostr.build
    let part2 = reqwest::multipart::Part::bytes(bytes)
        .file_name(format!("ulendo.{}", ext))
        .mime_str(&mime)
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
    let form2 = reqwest::multipart::Form::new().part("fileToUpload", part2);

    let resp2 = client
        .post("https://nostr.build/api/v2/nip96/upload")
        .multipart(form2)
        .send()
        .await;

    if let Ok(r2) = resp2 {
        if r2.status().is_success() {
            if let Ok(j2) = r2.json::<serde_json::Value>().await {
                let url = j2["nip94_event"]["tags"]
                    .as_array()
                    .and_then(|tags| tags.iter().find(|t| t[0] == "url"))
                    .and_then(|t| t.get(1).and_then(|v| v.as_str()))
                    .or_else(|| j2["data"].get(0).and_then(|d| d["url"].as_str()));
                if let Some(url) = url {
                    if url.starts_with("https://") {
                        return Ok(Json(json!({ "url": url, "provider": "nostr.build" })));
                    }
                }
            }
        }
    }

    Err(AppError::BadRequest("all upload providers failed".into()))
}
