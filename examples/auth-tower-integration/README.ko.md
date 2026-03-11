# Auth Tower Integration 예제 (생태계 방식)

**대안적 접근법**: 인증을 위한 Tower Service 레이어 통합.

---

## 이 예제가 보여주는 것

이 예제는 **인증을 위해 Tower 미들웨어를 Ranvier와 통합하는 방법**을 보여줍니다:

1. **Tower HTTP 미들웨어** (`tower-http::auth::RequireAuthorizationLayer`)
2. **커스텀 `AuthorizeRequest` 구현** (고수준 API)
3. **수동 Layer + Service 구현** (저수준, 교육용)
4. **Tower + Ranvier 통합 패턴** (Tower가 검증, Ranvier가 비즈니스 로직 처리)
5. **Request extensions** (Tower는 `AuthContext`를 `request.extensions()`에 저장, Bus 아님)

---

## 왜 Tower 통합인가? (생태계 방식)

### ✅ **생태계 호환성**

검증된 `tower-http` 미들웨어를 수정 없이 활용:

```rust
use tower::ServiceBuilder;
use tower_http::{cors::CorsLayer, trace::TraceLayer, timeout::TimeoutLayer};

let service = ServiceBuilder::new()
    .layer(CorsLayer::permissive())
    .layer(TraceLayer::new_for_http())
    .layer(TimeoutLayer::new(Duration::from_secs(30)))
    .layer(jwt_auth_layer(secret))  // 커스텀 인증 레이어
    .service(ranvier_adapter);
```

**장점**:
- **기존 레이어 재사용**: CORS, Trace, Timeout, RateLimit, Compression
- **재발명 불필요**: Tower가 이미 잘하는 것을 다시 작성하지 않음
- **프로덕션 검증**: Tower는 많은 Rust 웹 서비스를 구동

### ✅ **팀 지식 전이**

팀이 이미 Tower를 알고 있다면, 학습 곡선 최소화:

```rust
// 익숙한 Tower 패턴
impl<B> AuthorizeRequest<B> for JwtAuthorizer {
    fn authorize(&mut self, request: &mut Request<B>) -> Result<(), Response<Self::ResponseBody>> {
        // 표준 Tower 인증 로직
        let auth_header = request.headers().get("authorization")?;
        let token = extract_bearer_token(auth_header)?;
        let auth_ctx = validate_jwt(token, &self.secret)?;
        request.extensions_mut().insert(auth_ctx);
        Ok(())
    }
}
```

**장점**:
- **익숙한 패턴**: Tower 경험을 직접 적용 가능
- **적은 교육**: Tower 기반 인증에 Ranvier 인증 패턴을 배울 필요 없음
- **점진적 마이그레이션**: Tower 인증을 유지하면서 비즈니스 로직에 Ranvier 추가

### ✅ **점진적 마이그레이션 경로**

기존 Tower 앱이 있다면, 점진적으로 마이그레이션:

**1단계**: HTTP 관심사(CORS, 인증, 추적)는 Tower 유지
```rust
let service = ServiceBuilder::new()
    .layer(cors_layer)
    .layer(jwt_auth_layer)
    .service(existing_tower_app);  // 여전히 순수 Tower
```

**2단계**: 새 비즈니스 로직에 Ranvier 추가
```rust
let service = ServiceBuilder::new()
    .layer(cors_layer)
    .layer(jwt_auth_layer)
    .service(ranvier_adapter);  // 새 엔드포인트는 Ranvier 사용
```

**3단계**: 비즈니스 로직을 Ranvier transition으로 점진적 이동
```rust
// 이전: Tower Service
async fn handler(req: Request) -> Response { ... }

// 이후: Ranvier Transition
#[transition]
async fn handler(input: Req, res: &(), bus: &mut Bus) -> Outcome<Resp, E> { ... }
```

---

## 트레이드오프

### **장점** ✅

- **Tower 생태계 재사용**: CORS, Trace, Timeout, RateLimit, Compression
- **팀 지식**: 기존 Tower 경험을 직접 적용 가능
- **검증된 솔루션**: Tower 미들웨어는 프로덕션에서 검증됨
- **점진적 마이그레이션**: Tower 인증을 유지하면서 Ranvier를 점진적으로 추가

### **단점** ❌

- **Schematic 시각화 불가**: Tower 레이어는 Ranvier의 `schematic.json`에서 불투명함
  - VSCode Circuit 뷰에서 인증 흐름을 볼 수 없음
  - 디버깅하려면 Tower 미들웨어 체인을 이해해야 함

- **AuthContext가 Bus에 없음**: Tower는 컨텍스트를 `request.extensions()`에 저장
  - extensions에서 추출 → Bus에 넣는 어댑터 필요
  - Transition이 Bus에서 직접 `AuthContext`에 접근할 수 없음 (어댑터가 수행하지 않는 한)

- **더 많은 보일러플레이트**: 커스텀 Tower 레이어는 Ranvier transition보다 더 많은 코드 필요
  - 저수준: 수동 Layer + Service에 150+ 줄
  - 고수준: `AuthorizeRequest` 구현에 50+ 줄
  - Ranvier transition과 비교: 20줄

---

## 언제 이 접근법을 사용하나

**Tower 통합 선택** 경우:
- **기존 Tower 앱**이 있고 Ranvier를 점진적으로 추가하고 싶을 때
- 팀이 **이미 Tower를 알고** 있고 그 지식을 활용하고 싶을 때
- Ranvier가 제공하지 않는 **특정 Tower 미들웨어**가 필요할 때 (예: 커스텀 CORS, rate limiting)
- Tower를 사용하는 **다른 Rust 웹 프레임워크에서 마이그레이션** 중일 때 (actix-web, axum)

**순수 Ranvier 선택** (`examples/auth-transition/`) 경우:
- **새 프로젝트를 시작**하고 전체 Ranvier 이점을 원할 때
- 인증 흐름의 **Schematic 시각화**를 VSCode에서 원할 때
- **Bus 기반 컨텍스트 전파**를 선호할 때 (타입 안전, 명시적)
- **쉬운 테스트**를 원할 때 (transition을 독립적으로 단위 테스트)

**비교 참고**: [`docs/guides/auth-comparison.md`](../../docs/guides/auth-comparison.md)

---

## 실행 방법

### 1. 빌드

```bash
cd examples/auth-tower-integration
cargo build
```

### 2. 예제 실행

```bash
cargo run
```

예제는 다음을 시연합니다:
- ✅ 유효한 admin 토큰 → Tower 검증, Ranvier 요청 처리 → 성공
- ❌ 유효하지 않은 토큰 → Tower 거부(401 반환) → Ranvier에 도달하지 않음

### 3. (선택 사항) 사용자 정의 JWT 시크릿 설정

```bash
export JWT_SECRET="your-secret-key"
cargo run
```

---

## 예상 출력

```
INFO Starting auth-tower-integration example (Tower + Ranvier)
INFO Tower auth layer configured (JWT validation)
INFO Ranvier pipeline configured (business logic)

In production, you'd wrap this with Tower ServiceBuilder:
  ServiceBuilder::new()
    .layer(CorsLayer::permissive())
    .layer(jwt_auth_layer(secret))
    .service(ranvier_adapter)

=== Demo Execution ===

Scenario 1: Tower validates token, Ranvier handles request
INFO User authenticated successfully (Tower verified token), user_id="alice"
✅ Success: {"message":"Hello, alice! (Verified by Tower)","user_id":"alice","roles":["admin","user"],"timestamp":"2026-03-11T...","verified_by":"Tower middleware"}

Scenario 2: Tower rejects invalid token (doesn't reach Ranvier)
  In production, Tower returns 401 before calling Ranvier
  Ranvier pipeline never executes
```

---

## 코드 구조

```
examples/auth-tower-integration/
├── Cargo.toml          # 의존성 (ranvier, tower, tower-http, jsonwebtoken)
├── src/
│   ├── main.rs         # Tower + Ranvier 통합 패턴
│   ├── tower_auth.rs   # 두 가지 Tower 인증 구현 (고수준 + 저수준)
│   └── auth.rs         # AuthContext, AuthError, JWT 검증
├── README.md           # 영문 버전
├── README.ko.md        # 이 파일
└── .env.example        # JWT 시크릿 예제
```

### 두 가지 Tower 통합 접근법

이 예제는 Tower 인증을 통합하는 **두 가지 방법**을 보여줍니다:

#### **옵션 A: 수동 Layer + Service (교육용)**

Tower 내부를 보여주는 저수준 구현:

```rust
pub struct AuthLayer { secret: String }
pub struct AuthService<S> { inner: S, secret: String }

impl<S> Layer<S> for AuthLayer {
    type Service = AuthService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        AuthService { inner, secret: self.secret.clone() }
    }
}

impl<S, B> Service<Request<B>> for AuthService<S>
where
    S: Service<Request<B>, Response = Response<BoxBody>>,
{
    fn call(&mut self, mut req: Request<B>) -> Self::Future {
        // JWT 검증 로직...
        self.inner.call(req)
    }
}
```

**사용 시기**: 교육 목적, 요청/응답 흐름에 대한 전체 제어.

#### **옵션 B: AuthorizeRequest 트레잇 (권장)**

`tower-http::auth::RequireAuthorizationLayer`를 사용한 고수준 API:

```rust
#[derive(Clone)]
pub struct JwtAuthorizer {
    pub secret: String,
}

impl<B> AuthorizeRequest<B> for JwtAuthorizer {
    type ResponseBody = String;

    fn authorize(&mut self, request: &mut Request<B>) -> Result<(), Response<Self::ResponseBody>> {
        let auth_header = request.headers().get("authorization")...;
        let token = extract_bearer_token(auth_header)?;
        let auth_ctx = validate_jwt(token, &self.secret)?;
        request.extensions_mut().insert(auth_ctx);
        Ok(())
    }
}

pub fn jwt_auth_layer(secret: String) -> RequireAuthorizationLayer<JwtAuthorizer> {
    RequireAuthorizationLayer::new(JwtAuthorizer { secret })
}
```

**사용 시기**: 프로덕션 (권장), 적은 보일러플레이트, 유지보수 용이.

---

## 다음 단계

### 이 예제 확장하기

1. **CORS 레이어 추가**
   ```rust
   use tower_http::cors::CorsLayer;

   let service = ServiceBuilder::new()
       .layer(CorsLayer::permissive())
       .layer(jwt_auth_layer(secret))
       .service(ranvier_adapter);
   ```

2. **요청 추적 추가**
   ```rust
   use tower_http::trace::TraceLayer;

   let service = ServiceBuilder::new()
       .layer(TraceLayer::new_for_http())
       .layer(jwt_auth_layer(secret))
       .service(ranvier_adapter);
   ```

3. **속도 제한 추가** (`tower::limit::RateLimitLayer` 사용)
   ```rust
   use tower::limit::RateLimitLayer;
   use std::time::Duration;

   let service = ServiceBuilder::new()
       .layer(RateLimitLayer::new(100, Duration::from_secs(60)))
       .layer(jwt_auth_layer(secret))
       .service(ranvier_adapter);
   ```

4. **타임아웃 보호 추가**
   ```rust
   use tower_http::timeout::TimeoutLayer;

   let service = ServiceBuilder::new()
       .layer(TimeoutLayer::new(Duration::from_secs(30)))
       .layer(jwt_auth_layer(secret))
       .service(ranvier_adapter);
   ```

### 접근법 비교

- **순수 Ranvier 접근법**: [`examples/auth-transition/`](../auth-transition/) 참고
  - Transition 기반 인증 시연 (새 프로젝트에 권장)
  - Bus 기반 컨텍스트 전파, Schematic 시각화, 쉬운 테스트

- **자세한 비교**: [`docs/guides/auth-comparison.md`](../../docs/guides/auth-comparison.md) 참고
  - 기능 비교 표
  - 성능 분석
  - 마이그레이션 경로 (Tower → Ranvier, Ranvier → Tower)
  - 언제 어떤 접근법을 사용할지

### 프로덕션 고려 사항

프로덕션에 배포할 때:

1. **환경 변수 사용** JWT 시크릿에 (하드코딩 금지)
2. **HTTPS 추가** 토큰 가로채기 방지 (로드 밸런서에서 TLS 종료 사용)
3. **토큰 취소** (Tower 미들웨어에서 블랙리스트/데이터베이스 확인)
4. **토큰 만료** (단기 액세스 토큰: 15분, 장기 갱신: 7일)
5. **속도 제한** (인증 엔드포인트에 대한 무차별 대입 공격 방지)
6. **CORS 구성** (다른 도메인에서 웹 클라이언트 서비스 시)
7. **메트릭** (인증 성공/실패율을 `tower-http::metrics`로 추적)
8. **알림** (비정상적인 인증 패턴, 높은 실패율)

---

## 관련 문서

- [PHILOSOPHY.md](../../PHILOSOPHY.md) — "Opinionated Core, Flexible Edges" 원칙
- [DESIGN_PRINCIPLES.md](../../DESIGN_PRINCIPLES.md) — 아키텍처 결정 기록
- [docs/guides/auth-comparison.md](../../docs/guides/auth-comparison.md) — Tower vs Ranvier 인증 비교
- [`examples/auth-transition/`](../auth-transition/) — 순수 Ranvier 접근법 (권장)

---

## 라이선스

이 예제는 Ranvier 프로젝트의 일부이며 동일한 라이선스를 따릅니다.
