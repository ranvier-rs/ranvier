# Reference E-commerce Order

Public reference app for a workflow-heavy Ranvier service.

This example is the reference point for users who need more than CRUD:
- saga compensation
- audit-aware flow shape
- tenant-aware order listing
- RFC 7807-style error handling

## Endpoints

- `POST /login`
- `POST /orders`
- `GET /orders`
- `GET /orders/:id`
- `GET /inventory`

## Run

```bash
cargo run -p reference-ecommerce-order
```

## Audience

Use this example when you want:
- a public reference app focused on workflow and orchestration
- a larger example after `order-processing-demo`
- a realistic service shape beyond simple CRUD

Move to a different example if you need:
- a smaller JWT CRUD baseline -> `reference-todo-api`
- an admin-style backend -> `admin-crud-demo`
- a public-only fullstack admin surface -> `reference-fullstack-admin`

## Architecture

```text
CreateOrder -> ProcessPayment -> ReserveInventory -> ScheduleShipping
                 ↓ (comp)          ↓ (comp)
             RefundPayment    ReleaseInventory
```

## Expected Outcomes

- successful order creation with 4-stage workflow
- compensation on payment/inventory failure paths
- tenant-aware order list and detail retrieval
