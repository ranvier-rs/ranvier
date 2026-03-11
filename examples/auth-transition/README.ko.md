# Auth Transition 예제 (Ranvier 방식)

**권장 접근법**: Ranvier의 핵심 패러다임을 사용한 Transition 기반 인증.

---

## 이 예제가 보여주는 것

이 예제는 Transition/Outcome/Bus 패러다임을 사용하여 **순수 Ranvier로 인증을 구현하는 방법**을 보여줍니다:

1. **JWT 인증** (`#[transition]` 매크로 사용)
2. **역할 기반 권한 부여** (RBAC)
3. **Bus를 통한 컨텍스트 전파** (타입 안전, 명시적)
4. **인증 흐름의 Schematic 시각화**
5. **조합 가능한 파이프라인** (단계 추가/제거 용이)

---

## 왜 Transition 기반 인증인가? (Ranvier 권장)

### ✅ **Bus 기반 컨텍스트 전파**

`AuthContext`는 인증 성공 후 자동으로 Bus에 저장되어 모든 다운스트림 transition에서 사용할 수 있습니다:

```rust
// authenticate transition이 AuthContext를 반환 → Bus에 저장됨
#[transition]
async fn authenticate(req: Request) -> Outcome<AuthContext, AuthError> {
    let auth_ctx = validate_jwt(token)?;
    Outcome::Next(auth_ctx)  // 자동으로 Bus에 저장
}

// authorize가 &AuthContext를 받음 → Bus에서 자동 주입
#[transition]
async fn authorize(auth: &AuthContext) -> Outcome<(), AuthError> {
    if !auth.roles.contains(&"admin".into()) {
        return Outcome::Fault(AuthError::Unauthorized("admin".into()));
    }
    Outcome::Next(())
}
```

**장점**:
- **타입 안전**: 컴파일러가 `authorize` 실행 전 `AuthContext` 존재 보장
- **명시적**: 숨겨진 전역 변수나 request extension 없음
- **테스트 가능**: 테스트 시 모의 `AuthContext`를 Bus에 주입

### ✅ **Schematic 시각화**

전체 인증 흐름이 `schematic.json`에 표현되고 VSCode Circuit 뷰에서 볼 수 있습니다:

```
authenticate → authorize → protected_handler
     ↓              ↓              ↓
AuthContext    admin 역할     AuthContext를
(Bus로)        확인          응답에 사용
```

**장점**:
- **시각적 디버깅**: 한눈에 정확한 흐름 파악
- **문서화**: Schematic이 문서입니다 (항상 최신 상태)
- **팀 커뮤니케이션**: 비기술 이해관계자도 흐름 이해 가능

### ✅ **테스트 용이성**

각 transition을 독립적으로 단위 테스트할 수 있습니다:

```rust
#[tokio::test]
async fn test_authorize_success() {
    let mut bus = Bus::new();
    bus.write(AuthContext {
        user_id: "alice".into(),
        roles: vec!["admin".into()],
    });

    let result = authorize(&AuthContext { ... }).await;
    assert!(result.is_ok());
}
```

**장점**:
- **빠른 테스트**: HTTP 서버 불필요
- **격리된 테스트**: 인증 로직을 JWT 파싱과 독립적으로 테스트
- **모의 친화적**: 엣지 케이스를 위해 어떤 `AuthContext`든 주입 가능

### ✅ **조합 가능성**

파이프라인에 추가 단계를 쉽게 확장할 수 있습니다:

```rust
// 감사 로깅 추가
let pipeline = Axon::simple()
    .pipe(authenticate, audit_log, authorize, protected_handler)
    .build();

// 구독 확인 추가
let pipeline = Axon::simple()
    .pipe(authenticate, check_subscription, authorize, protected_handler)
    .build();

// 병렬 검사 (역할 + 구독)
let pipeline = Axon::simple()
    .pipe(authenticate)
    .parallel(authorize, check_subscription)
    .pipe(protected_handler)
    .build();
```

**장점**:
- **점진적 기능 추가**: 기존 코드를 다시 작성하지 않고 단계 추가
- **명확한 의도**: 파이프라인 구조가 비즈니스 로직 흐름을 보여줌
- **병렬 실행**: 독립적인 검사를 동시에 실행

---

## 실행 방법

### 1. 의존성 설치

```bash
cd examples/auth-transition
cargo build
```

### 2. 예제 실행

```bash
cargo run
```

예제는 데모 시나리오를 실행합니다:
- ✅ 유효한 admin 토큰 → 성공
- ❌ 유효한 토큰, admin 역할 없음 → 권한 부여 오류
- ❌ Authorization 헤더 누락 → 인증 오류
- ❌ 유효하지 않은 토큰 → JWT 검증 오류

### 3. (선택 사항) 사용자 정의 JWT 시크릿 설정

```bash
export JWT_SECRET="your-secret-key"
cargo run
```

---

## 예상 출력

```
INFO Starting auth-transition example (Ranvier Way)
INFO Schematic exported to schematic.json

=== Demo Execution ===

Scenario 1: Valid admin token
INFO User authenticated successfully, user_id="alice", roles=["admin", "user"]
INFO Authorization successful, user_id="alice", role="admin"
INFO Protected handler executed for authenticated user, user_id="alice"
✅ Success: {"message":"Hello, alice!","user_id":"alice","roles":["admin","user"],"timestamp":"2026-03-11T..."}

Scenario 2: Valid token, no admin role
INFO User authenticated successfully, user_id="bob", roles=["user"]
WARN Authorization failed: missing required role, user_id="bob", required_role="admin", actual_roles=["user"]
✅ Expected error: Auth error: Unauthorized: requires role admin

Scenario 3: Missing Authorization header
✅ Expected error: Auth error: Missing authorization header

Scenario 4: Invalid token
✅ Expected error: Auth error: Invalid token: ...
```

---

## 코드 구조

```
examples/auth-transition/
├── Cargo.toml          # 의존성 (ranvier, jsonwebtoken, serde, tokio)
├── src/
│   ├── main.rs         # 파이프라인 정의 + 데모 실행
│   └── auth.rs         # AuthContext, AuthError, JWT 검증 헬퍼
├── README.md           # 영문 버전
├── README.ko.md        # 이 파일
└── .env.example        # JWT 시크릿 예제
```

---

## 다음 단계

### 이 예제 확장하기

1. **더 많은 역할 추가**: 특정 역할 확인 (예: `"moderator"`, `"billing"`)
   ```rust
   #[transition]
   async fn require_role(auth: &AuthContext, role: &str) -> Outcome<(), AuthError> {
       if !auth.roles.contains(&role.to_string()) {
           return Outcome::Fault(AuthError::Unauthorized(role.into()));
       }
       Outcome::Next(())
   }
   ```

2. **감사 로깅 추가**: 모든 인증 시도 기록
   ```rust
   #[transition]
   async fn audit_log(auth: &AuthContext, req: &Request) -> Outcome<(), Never> {
       tracing::info!(user_id = %auth.user_id, path = %req.uri, "Auth request");
       Outcome::Next(())
   }
   ```

3. **구독 확인 추가**: 사용자가 활성 구독을 가지고 있는지 확인
   ```rust
   #[transition]
   async fn check_subscription(auth: &AuthContext, db: &PgPool) -> Outcome<(), SubError> {
       let sub = fetch_subscription(db, &auth.user_id).await?;
       if !sub.is_active() {
           return Outcome::Fault(SubError::Expired);
       }
       Outcome::Next(())
   }
   ```

4. **토큰 갱신 추가**: 만료된 토큰을 우아하게 처리
   ```rust
   #[transition]
   async fn refresh_if_expired(auth: AuthContext, req: &Request) -> Outcome<AuthContext, AuthError> {
       if auth.is_expired() {
           let new_token = refresh_token(&req)?;
           return Outcome::Next(new_token);
       }
       Outcome::Next(auth)
   }
   ```

### 다른 접근법과 비교

- **Tower 통합**: `examples/auth-tower-integration/` 참고
  인증을 위해 Tower Service 레이어 사용 시연 (생태계 방식)

- **비교 가이드**: `docs/guides/auth-comparison.md` 참고
  자세한 기능 비교, 성능 분석, 마이그레이션 경로

### 프로덕션 고려 사항

프로덕션에 배포할 때:

1. **환경 변수 사용** JWT 시크릿에 (하드코딩 금지)
2. **속도 제한 추가** 무차별 대입 공격 방지
3. **토큰 취소 추가** (블랙리스트/데이터베이스 확인)
4. **HTTPS 사용** 토큰 가로채기 방지
5. **토큰 만료 설정** (단기: 15분 액세스, 장기: 7일 갱신)
6. **CORS 추가** 다른 도메인의 웹 클라이언트 서비스 시
7. **메트릭 추가** (인증 성공/실패율)
8. **알림 추가** (비정상적인 인증 패턴)

---

## 관련 문서

- [PHILOSOPHY.md](../../PHILOSOPHY.md) — "Opinionated Core, Flexible Edges" 원칙
- [DESIGN_PRINCIPLES.md](../../DESIGN_PRINCIPLES.md) — 아키텍처 결정 기록
- [docs/guides/auth-comparison.md](../../docs/guides/auth-comparison.md) — Transition vs Tower 비교

---

## 라이선스

이 예제는 Ranvier 프로젝트의 일부이며 동일한 라이선스를 따릅니다.
