use actix_web::{App, HttpRequest, HttpResponse, HttpServer, web};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Claims {
    sub: String,
    roles: Vec<String>,
    exp: usize,
}

#[derive(Serialize)]
struct AuthResponse {
    subject: String,
    message: String,
}

async fn protected_handler(req: HttpRequest) -> HttpResponse {
    let auth_header = req
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));

    let token = match auth_header {
        Some(t) => t,
        None => return HttpResponse::Unauthorized().finish(),
    };

    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = true;

    let token_data = match decode::<Claims>(
        token,
        &DecodingKey::from_secret(b"bench-secret-key"),
        &validation,
    ) {
        Ok(data) => data,
        Err(_) => return HttpResponse::Unauthorized().finish(),
    };

    if !token_data.claims.roles.contains(&"admin".to_string()) {
        return HttpResponse::Forbidden().finish();
    }

    HttpResponse::Ok().json(AuthResponse {
        subject: token_data.claims.sub,
        message: "Access Granted via Actix-web".to_string(),
    })
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if args.len() > 1 && args[1] == "gen-token" {
        let token = generate_bench_token("bench-secret-key").unwrap();
        print!("{}", token);
        return Ok(());
    }

    println!("Starting Actix-web Benchmark Server (Scenario 2: Auth) on 0.0.0.0:5001");
    HttpServer::new(|| App::new().route("/protected", web::get().to(protected_handler)))
        .bind("0.0.0.0:5001")?
        .run()
        .await
}

fn generate_bench_token(secret: &str) -> anyhow::Result<String> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as usize;

    let claims = Claims {
        sub: "bench-user".to_string(),
        roles: vec!["admin".to_string(), "user".to_string()],
        exp: now + 3600,
    };

    let token = encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )?;

    Ok(token)
}
