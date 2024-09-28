# Notification Service

This service provides WebSocket and REST endpoints for real-time notifications, running inside a Kubernetes environment with RabbitMQ for message distribution.

## Features

- WebSocket for real-time updates
- REST API for publishing messages
- RabbitMQ integration for message distribution
- JWT authentication for WebSocket connections
- Kubernetes deployment-ready

## Setup

### Prerequisites

- Rust (for local development)
- Docker (for building and deploying)
- Kubernetes cluster
- RabbitMQ

### Build and Deploy

1. **Build Docker Image:**

   ```bash
   docker build -t notification-service .
   ```

2. **Deploy to Kubernetes:**

   Apply the Deployment and Service manifests:

   ```bash
   kubectl apply -f notification-service-deployment.yaml
   kubectl apply -f notification-service-service.yaml
   ```

## Usage

### WebSocket Subscription

```bash
ws://localhost:8001/api/v1/namespaces/default/services/notification-service-service:http/proxy/notification/ws/{channel_name}?token={JWT_TOKEN}
```

### Publish Message

```bash
curl -X POST http://localhost:8001/api/v1/namespaces/default/services/notification-service-service:http/proxy/notification/channels/{channel_name}/publish \
    -H "Content-Type: application/json" \
    -d '{"message": "Hello, World!"}'
```

### Swagger Documentation

Access Swagger UI at:

```
http://localhost:8001/api/v1/namespaces/default/services/notification-service-service:http/proxy/notification/swagger-ui
```

### Access Locally via kubectl proxy

Start the proxy:

```bash
kubectl proxy
```

Then use `localhost:8001` to access WebSocket or REST endpoints.
