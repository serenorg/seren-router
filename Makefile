# seren-router image publishing helpers.

ifneq (,$(wildcard .env))
include .env
endif

SHELL := /bin/sh

GHCR_REGISTRY ?= ghcr.io
GHCR_OWNER ?= serenorg
IMAGE_NAME ?= $(notdir $(CURDIR))
GHCR_IMAGE ?= $(GHCR_REGISTRY)/$(GHCR_OWNER)/$(IMAGE_NAME)
CARGO_FEATURES ?= production
PORT ?= 8000
BUILD_TIMESTAMP := $(shell TZ=UTC date +%Y%m%d%H%M)
VERSION ?= $(shell git describe --tags --always --dirty 2>/dev/null || echo dev)
DOCKER_BUILDKIT ?= 1
BUILDKIT_PROGRESS ?= plain
DOCKER_PLATFORMS ?= linux/amd64,linux/arm64
BUILDX_BUILDER ?= multiplatform

UNAME_M := $(shell uname -m)
ifeq ($(UNAME_M),arm64)
LOCAL_ARCH := arm64
else ifeq ($(UNAME_M),aarch64)
LOCAL_ARCH := arm64
else
LOCAL_ARCH := amd64
endif

LOCAL_TAG ?= latest-$(LOCAL_ARCH)

.PHONY: docker-build docker-run docker-login-ghcr docker-push-ghcr docker-buildx-setup docker-build-multiplatform-ghcr help

docker-build:
	DOCKER_BUILDKIT=$(DOCKER_BUILDKIT) docker build \
		--progress=$(BUILDKIT_PROGRESS) \
		--build-arg CARGO_FEATURES="$(CARGO_FEATURES)" \
		-t $(GHCR_IMAGE):$(BUILD_TIMESTAMP)-$(LOCAL_ARCH) \
		-t $(GHCR_IMAGE):$(LOCAL_TAG) \
		.
	@echo "Built $(GHCR_IMAGE):$(BUILD_TIMESTAMP)-$(LOCAL_ARCH)"
	@echo "Built $(GHCR_IMAGE):$(LOCAL_TAG)"

docker-run:
	docker run --rm -it \
		-p $(PORT):8000 \
		-e RUST_LOG=$${RUST_LOG:-info} \
		$(GHCR_IMAGE):$(LOCAL_TAG)

docker-login-ghcr:
	@printf "GitHub username: "; \
	read username; \
	docker login $(GHCR_REGISTRY) -u "$$username"

docker-push-ghcr:
	@# Discover the most recently built timestamped local tag from `docker
	@# images` rather than re-computing BUILD_TIMESTAMP (which would drift
	@# whenever build and push run in separate `make` invocations).
	@latest=$$(docker images --format '{{.Repository}}:{{.Tag}}' \
		| grep '^$(GHCR_IMAGE):[0-9]\{12\}-$(LOCAL_ARCH)$$' \
		| sort -r | head -1 | sed 's/.*://'); \
	if [ -z "$$latest" ]; then \
		echo "error: no $(GHCR_IMAGE):YYYYMMDDHHMM-$(LOCAL_ARCH) tag found. Run 'make docker-build' first." >&2; \
		exit 1; \
	fi; \
	echo "Pushing $(GHCR_IMAGE):$$latest"; \
	docker push $(GHCR_IMAGE):$$latest && \
	docker push $(GHCR_IMAGE):$(LOCAL_TAG) && \
	docker tag $(GHCR_IMAGE):$(LOCAL_TAG) $(GHCR_IMAGE):$(VERSION) && \
	docker push $(GHCR_IMAGE):$(VERSION)
	@# Update the multi-arch :latest manifest list. Composes from
	@# whichever per-arch tags happen to be present so a single-arch push
	@# still produces a working :latest; once the other arch ships the
	@# command picks both up automatically. The `||` guard keeps the push
	@# green if neither arch tag exists yet (first-time bootstrap).
	@echo "Updating multi-arch :latest manifest..."
	@docker buildx imagetools create \
		-t $(GHCR_IMAGE):latest \
		$$(for arch in amd64 arm64; do \
			docker buildx imagetools inspect $(GHCR_IMAGE):latest-$$arch >/dev/null 2>&1 \
				&& echo $(GHCR_IMAGE):latest-$$arch; \
		done) \
		|| echo "  (skipped - no per-arch latest- tags found on registry yet)"

docker-buildx-setup:
	@docker buildx inspect $(BUILDX_BUILDER) >/dev/null 2>&1 || \
		docker buildx create --name $(BUILDX_BUILDER) --driver docker-container --use
	@docker buildx inspect --bootstrap $(BUILDX_BUILDER)

docker-build-multiplatform-ghcr: docker-buildx-setup
	DOCKER_BUILDKIT=$(DOCKER_BUILDKIT) docker buildx build \
		--progress=$(BUILDKIT_PROGRESS) \
		--build-arg CARGO_FEATURES="$(CARGO_FEATURES)" \
		--platform $(DOCKER_PLATFORMS) \
		-t $(GHCR_IMAGE):$(BUILD_TIMESTAMP) \
		-t $(GHCR_IMAGE):$(VERSION) \
		-t $(GHCR_IMAGE):latest \
		--push \
		.
	@echo "Pushed $(GHCR_IMAGE):$(BUILD_TIMESTAMP)"
	@echo "Pushed $(GHCR_IMAGE):$(VERSION)"
	@echo "Pushed $(GHCR_IMAGE):latest"

help:
	@echo "Image commands:"
	@echo "  make docker-build                    Build local image tags"
	@echo "  make docker-run                      Run the local image on PORT=$(PORT)"
	@echo "  make docker-login-ghcr               Login to $(GHCR_REGISTRY)"
	@echo "  make docker-push-ghcr                Push local image tags to GHCR"
	@echo "  make docker-build-multiplatform-ghcr Build and push multi-platform image"
	@echo ""
	@echo "Common overrides:"
	@echo "  GHCR_OWNER=$(GHCR_OWNER)"
	@echo "  IMAGE_NAME=$(IMAGE_NAME)"
	@echo "  CARGO_FEATURES=$(CARGO_FEATURES)"
	@echo "  DOCKER_PLATFORMS=$(DOCKER_PLATFORMS)"
