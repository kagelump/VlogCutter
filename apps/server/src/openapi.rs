//! The hand-maintained OpenAPI document served at `/api/v1/openapi.json`.

use super::*;

pub fn openapi_document() -> serde_json::Value {
    let mut document = base_openapi_document();
    review_target_http::openapi_extend(&mut document);
    review_session_http::openapi_extend(&mut document);
    review_transcript_http::openapi_extend(&mut document);
    review_discussion_http::openapi_extend(&mut document);
    document
}

pub(super) fn base_openapi_document() -> serde_json::Value {
    serde_json::json!({
        "openapi": "3.1.0",
        "info": {
            "title": "RCD VlogCutter Server API",
            "version": APPLICATION_VERSION,
            "description": "Private single-user companion API for authenticated project synchronization, browser review, storage/library administration, durable jobs, derived-media streaming, and verified backups. Originals are never HTTP content sources."
        },
        "paths": {
            "/api/v1/version": {
                "get": {
                    "operationId": "getServerVersion",
                    "responses": { "200": { "$ref": "#/components/responses/Version" } }
                }
            },
            "/api/v1/ready": {
                "get": {
                    "operationId": "getServerReadiness",
                    "responses": {
                        "200": { "$ref": "#/components/responses/Readiness" },
                        "503": { "$ref": "#/components/responses/Readiness" }
                    }
                }
            },
            "/api/v1/auth/bootstrap/exchange": {
                "post": {
                    "operationId": "exchangeBootstrapCode",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/BootstrapExchangeRequest" }
                            }
                        }
                    },
                    "responses": {
                        "200": { "$ref": "#/components/responses/SessionCreated" },
                        "401": { "$ref": "#/components/responses/Error" }
                    }
                }
            },
            "/api/v1/auth/sessions": {
                "get": {
                    "operationId": "listSessions",
                    "responses": {
                        "200": { "$ref": "#/components/responses/Sessions" },
                        "401": { "$ref": "#/components/responses/Error" }
                    }
                }
            },
            "/api/v1/library/media": {
                "get": {
                    "operationId": "listLibraryMedia",
                    "parameters": [
                        { "name": "limit", "in": "query", "schema": { "type": "integer", "minimum": 1, "maximum": 200 } },
                        { "name": "sort", "in": "query", "schema": { "$ref": "#/components/schemas/LibrarySort" } },
                        { "name": "verificationState", "in": "query", "schema": { "$ref": "#/components/schemas/GlobalMediaVerificationState" } },
                        { "name": "storageRootId", "in": "query", "schema": { "type": "string" } },
                        { "name": "camera", "in": "query", "schema": { "type": "string" } },
                        { "name": "filenamePrefix", "in": "query", "schema": { "type": "string" } },
                        { "name": "availability", "in": "query", "schema": { "$ref": "#/components/schemas/NodeAvailabilityState" } },
                        { "name": "after", "in": "query", "schema": { "type": "string" } }
                    ],
                    "responses": {
                        "200": { "$ref": "#/components/responses/LibrarySearchPage" },
                        "400": { "$ref": "#/components/responses/Error" },
                        "401": { "$ref": "#/components/responses/Error" }
                    }
                }
            },
            "/api/v1/library/media/{mediaObjectId}": {
                "get": {
                    "operationId": "getLibraryMediaDetail",
                    "parameters": [
                        { "name": "mediaObjectId", "in": "path", "required": true, "schema": { "type": "string" } }
                    ],
                    "responses": {
                        "200": { "$ref": "#/components/responses/LibraryMediaDetail" },
                        "401": { "$ref": "#/components/responses/Error" },
                        "404": { "$ref": "#/components/responses/Error" }
                    }
                }
            },
            "/api/v1/projects": {
                "get": {
                    "operationId": "listProjects",
                    "parameters": [
                        { "name": "limit", "in": "query", "schema": { "type": "integer", "minimum": 1, "maximum": 200 } },
                        { "name": "after", "in": "query", "schema": { "type": "string" } }
                    ],
                    "responses": {
                        "200": { "$ref": "#/components/responses/ProjectPage" },
                        "400": { "$ref": "#/components/responses/Error" },
                        "401": { "$ref": "#/components/responses/Error" }
                    }
                }
            },
            "/api/v1/projects/{projectId}/assets": {
                "get": {
                    "operationId": "listProjectAssets",
                    "parameters": [
                        { "name": "projectId", "in": "path", "required": true, "schema": { "type": "string" } },
                        { "name": "limit", "in": "query", "schema": { "type": "integer", "minimum": 1, "maximum": 200 } },
                        { "name": "after", "in": "query", "schema": { "type": "string" } },
                        { "name": "search", "in": "query", "schema": { "type": "string" } }
                    ],
                    "responses": {
                        "200": { "$ref": "#/components/responses/ProjectAssetPage" },
                        "400": { "$ref": "#/components/responses/Error" },
                        "401": { "$ref": "#/components/responses/Error" },
                        "404": { "$ref": "#/components/responses/Error" }
                    }
                }
            },
            "/api/v1/projects/{projectId}/assets/add/preview": {
                "post": {
                    "operationId": "previewAddToProject",
                    "parameters": [
                        { "name": "projectId", "in": "path", "required": true, "schema": { "type": "string" } }
                    ],
                    "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/AddToProjectRequest" } } } },
                    "responses": {
                        "200": { "$ref": "#/components/responses/AddToProjectPreview" },
                        "400": { "$ref": "#/components/responses/Error" },
                        "401": { "$ref": "#/components/responses/Error" },
                        "404": { "$ref": "#/components/responses/Error" }
                    }
                }
            },
            "/api/v1/projects/{projectId}/assets/add": {
                "post": {
                    "operationId": "addToProject",
                    "parameters": [
                        { "name": "projectId", "in": "path", "required": true, "schema": { "type": "string" } }
                    ],
                    "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/AddToProjectRequest" } } } },
                    "responses": {
                        "200": { "$ref": "#/components/responses/AddToProjectResult" },
                        "400": { "$ref": "#/components/responses/Error" },
                        "401": { "$ref": "#/components/responses/Error" },
                        "404": { "$ref": "#/components/responses/Error" }
                    }
                }
            },
            "/api/v1/projects/{projectId}/assets/{assetId}/review": {
                "get": {
                    "operationId": "getProjectReviewAsset",
                    "parameters": [
                        { "name": "projectId", "in": "path", "required": true, "schema": { "type": "string" } },
                        { "name": "assetId", "in": "path", "required": true, "schema": { "type": "string" } }
                    ],
                    "responses": { "200": { "$ref": "#/components/responses/ProjectReviewAsset" }, "401": { "$ref": "#/components/responses/Error" }, "404": { "$ref": "#/components/responses/Error" } }
                }
            },
            "/api/v1/projects/{projectId}/assets/{assetId}/notes": {
                "post": { "operationId": "createReviewNote", "parameters": [
                    { "name": "projectId", "in": "path", "required": true, "schema": { "type": "string" } },
                    { "name": "assetId", "in": "path", "required": true, "schema": { "type": "string" } }
                ], "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/NoteMutationRequest" } } } }, "responses": { "200": { "$ref": "#/components/responses/ReviewMutationResult" }, "400": { "$ref": "#/components/responses/Error" }, "401": { "$ref": "#/components/responses/Error" } } }
            },
            "/api/v1/projects/{projectId}/assets/{assetId}/labels/add": {
                "post": { "operationId": "addReviewLabel", "parameters": [
                    { "name": "projectId", "in": "path", "required": true, "schema": { "type": "string" } },
                    { "name": "assetId", "in": "path", "required": true, "schema": { "type": "string" } }
                ], "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/LabelMutationRequest" } } } }, "responses": { "200": { "$ref": "#/components/responses/ReviewMutationResult" }, "400": { "$ref": "#/components/responses/Error" }, "401": { "$ref": "#/components/responses/Error" } } }
            },
            "/api/v1/projects/{projectId}/assets/{assetId}/labels/remove": {
                "post": { "operationId": "removeReviewLabel", "parameters": [
                    { "name": "projectId", "in": "path", "required": true, "schema": { "type": "string" } },
                    { "name": "assetId", "in": "path", "required": true, "schema": { "type": "string" } }
                ], "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/LabelMutationRequest" } } } }, "responses": { "200": { "$ref": "#/components/responses/ReviewMutationResult" }, "400": { "$ref": "#/components/responses/Error" }, "401": { "$ref": "#/components/responses/Error" } } }
            },
            "/api/v1/projects/{projectId}/assets/{assetId}/removed": {
                "post": { "operationId": "setReviewRemoved", "parameters": [
                    { "name": "projectId", "in": "path", "required": true, "schema": { "type": "string" } },
                    { "name": "assetId", "in": "path", "required": true, "schema": { "type": "string" } }
                ], "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/RemovedMutationRequest" } } } }, "responses": { "200": { "$ref": "#/components/responses/ReviewMutationResult" }, "400": { "$ref": "#/components/responses/Error" }, "401": { "$ref": "#/components/responses/Error" } } }
            },
            "/api/v1/projects/{projectId}/assets/{assetId}/rotation": {
                "post": { "operationId": "setReviewRotation", "parameters": [
                    { "name": "projectId", "in": "path", "required": true, "schema": { "type": "string" } },
                    { "name": "assetId", "in": "path", "required": true, "schema": { "type": "string" } }
                ], "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/RotationMutationRequest" } } } }, "responses": { "200": { "$ref": "#/components/responses/ReviewMutationResult" }, "400": { "$ref": "#/components/responses/Error" }, "401": { "$ref": "#/components/responses/Error" } } }
            },
            "/api/v1/auth/sessions/{sessionId}": { "delete": { "operationId": "revokeSession", "parameters": [{ "name": "sessionId", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": { "200": { "$ref": "#/components/responses/AdminPayload" }, "401": { "$ref": "#/components/responses/Error" } } } },
            "/api/v1/auth/devices": { "get": { "operationId": "listDevices", "responses": { "200": { "$ref": "#/components/responses/AdminPayload" }, "401": { "$ref": "#/components/responses/Error" } } } },
            "/api/v1/auth/devices/{credentialId}": { "delete": { "operationId": "revokeDevice", "parameters": [{ "name": "credentialId", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": { "200": { "$ref": "#/components/responses/AdminPayload" }, "401": { "$ref": "#/components/responses/Error" } } } },
            "/api/v1/admin/overview": { "get": { "operationId": "getAdminOverview", "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } } },
            "/api/v1/admin/audit": { "get": { "operationId": "listAdminAudit", "parameters": [{"name":"limit","in":"query","schema":{"type":"integer","minimum":1,"maximum":200}}, {"name":"before","in":"query","schema":{"type":"integer"}}], "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } } },
            "/api/v1/admin/storage-roots": {
                "get": { "operationId": "listAdminStorageRoots", "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } },
                "post": { "operationId": "createAdminStorageRoot", "requestBody": { "$ref": "#/components/requestBodies/AdminPayload" }, "responses": { "201": { "$ref": "#/components/responses/AdminPayload" } } }
            },
            "/api/v1/admin/storage-roots/{rootId}": { "patch": { "operationId": "updateAdminStorageRoot", "parameters": [{"name":"rootId","in":"path","required":true,"schema":{"type":"string"}}], "requestBody": { "$ref": "#/components/requestBodies/AdminPayload" }, "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } } },
            "/api/v1/admin/storage-bindings": { "get": { "operationId": "listAdminStorageBindings", "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } } },
            "/api/v1/admin/storage-roots/{rootId}/binding": { "put": { "operationId": "upsertAdminStorageBinding", "parameters": [{"name":"rootId","in":"path","required":true,"schema":{"type":"string"}}], "requestBody": { "$ref": "#/components/requestBodies/AdminPayload" }, "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } } },
            "/api/v1/admin/storage-roots/{rootId}/binding/confirmation": { "post": { "operationId": "confirmAdminStorageBinding", "parameters": [{"name":"rootId","in":"path","required":true,"schema":{"type":"string"}}], "requestBody": { "$ref": "#/components/requestBodies/AdminPayload" }, "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } } },
            "/api/v1/admin/storage-roots/{rootId}/binding/verify": { "post": { "operationId": "verifyAdminStorageBinding", "parameters": [{"name":"rootId","in":"path","required":true,"schema":{"type":"string"}}], "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } } },
            "/api/v1/admin/scans": {
                "get": { "operationId": "listAdminScans", "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } },
                "post": { "operationId": "requestAdminScan", "requestBody": { "$ref": "#/components/requestBodies/AdminPayload" }, "responses": { "202": { "$ref": "#/components/responses/AdminPayload" } } }
            },
            "/api/v1/admin/scans/{scanId}/diagnostics": { "get": { "operationId": "listAdminScanDiagnostics", "parameters": [
                { "name": "scanId", "in": "path", "required": true, "schema": { "type": "string" } },
                { "name": "limit", "in": "query", "schema": { "type": "integer", "minimum": 1, "maximum": 200 } },
                { "name": "after", "in": "query", "schema": { "type": "string" } }
            ], "responses": { "200": { "$ref": "#/components/responses/AdminPayload" }, "404": { "$ref": "#/components/responses/Error" } } } },
            "/api/v1/admin/scan-schedules": {
                "get": { "operationId": "listAdminScanSchedules", "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } },
                "post": { "operationId": "createAdminScanSchedule", "requestBody": { "$ref": "#/components/requestBodies/AdminPayload" }, "responses": { "201": { "$ref": "#/components/responses/AdminPayload" } } }
            },
            "/api/v1/admin/scan-schedules/{scheduleId}": { "patch": { "operationId": "updateAdminScanSchedule", "parameters": [{"name":"scheduleId","in":"path","required":true,"schema":{"type":"string"}}], "requestBody": { "$ref": "#/components/requestBodies/AdminPayload" }, "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } } },
            "/api/v1/admin/jobs": { "get": { "operationId": "listAdminJobs", "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } } },
            "/api/v1/admin/jobs/{jobId}": { "get": { "operationId": "getAdminJob", "parameters": [{"name":"jobId","in":"path","required":true,"schema":{"type":"string"}}], "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } } },
            "/api/v1/admin/jobs/{jobId}/cancel": { "post": { "operationId": "cancelAdminJob", "parameters": [{"name":"jobId","in":"path","required":true,"schema":{"type":"string"}}], "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } } },
            "/api/v1/admin/jobs/{jobId}/retry": { "post": { "operationId": "retryAdminJob", "parameters": [{"name":"jobId","in":"path","required":true,"schema":{"type":"string"}}], "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } } },
            "/api/v1/admin/jobs/{jobId}/callbacks": { "post": { "operationId": "subscribeJobCallback", "parameters": [{"name":"jobId","in":"path","required":true,"schema":{"type":"string"}}], "requestBody": {"required":true,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/CallbackSubscriptionRequest"}}}}, "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } } },
            "/api/v1/admin/job-callbacks": { "get": { "operationId": "listJobCallbacks", "parameters": [{"name":"target","in":"query","required":true,"schema":{"type":"string"}},{"name":"limit","in":"query","schema":{"type":"integer","minimum":1,"maximum":200}},{"name":"before","in":"query","schema":{"type":"integer","minimum":0}}], "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } } },
            "/api/v1/admin/job-callbacks/claim": { "post": { "operationId": "claimJobCallback", "requestBody": {"required":true,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/CallbackClaimRequest"}}}}, "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } } },
            "/api/v1/admin/job-callbacks/{deliveryId}/settle": { "post": { "operationId": "settleJobCallback", "parameters": [{"name":"deliveryId","in":"path","required":true,"schema":{"type":"string"}}], "requestBody": {"required":true,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/CallbackSettleRequest"}}}}, "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } } },
            "/api/v1/admin/sync": { "get": { "operationId": "listAdminSync", "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } } },
            "/api/v1/admin/backups": {
                "get": { "operationId": "listAdminBackups", "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } },
                "post": { "operationId": "requestAdminBackup", "requestBody": { "$ref": "#/components/requestBodies/AdminPayload" }, "responses": { "202": { "$ref": "#/components/responses/AdminPayload" } } }
            },
            "/api/v1/admin/backups/{backupId}": { "get": { "operationId": "getAdminBackup", "parameters": [{"name":"backupId","in":"path","required":true,"schema":{"type":"string"}}], "responses": { "200": { "$ref": "#/components/responses/AdminPayload" }, "404": { "$ref": "#/components/responses/Error" } } } },
            "/api/v1/admin/representations/batch": { "post": { "operationId": "requestAdminRepresentationBatch", "requestBody": { "$ref": "#/components/requestBodies/AdminPayload" }, "responses": { "202": { "$ref": "#/components/responses/AdminPayload" } } } }
        },
        "components": {
            "schemas": {
                "CallbackSubscriptionRequest": {"type":"object","additionalProperties":false,"required":["target"],"properties":{"target":{"type":"string","minLength":1,"maxLength":128,"pattern":"^[A-Za-z0-9._-]+$"}}},
                "CallbackClaimRequest": {"type":"object","additionalProperties":false,"required":["target","leaseSeconds"],"properties":{"target":{"type":"string","minLength":1,"maxLength":128,"pattern":"^[A-Za-z0-9._-]+$"},"leaseSeconds":{"type":"integer","minimum":1,"maximum":3600}}},
                "CallbackSettleRequest": {"type":"object","additionalProperties":false,"required":["leaseToken","succeeded"],"properties":{"leaseToken":{"type":"string"},"succeeded":{"type":"boolean"}}},
                "AdminPayload": { "type": "object", "additionalProperties": true },
                "BootstrapExchangeRequest": {
                    "type": "object",
                    "required": ["code"],
                    "properties": {
                        "code": { "type": "string" },
                        "hostname": { "type": ["string", "null"] }
                    }
                },
                "Error": {
                    "type": "object",
                    "required": ["requestId", "code"],
                    "properties": {
                        "requestId": { "type": "string" },
                        "code": { "type": "string" }
                    }
                },
                "ProtocolVersion": {
                    "type": "object",
                    "required": ["major", "minor"],
                    "properties": {
                        "major": { "type": "integer", "minimum": 0 },
                        "minor": { "type": "integer", "minimum": 0 }
                    }
                },
                "Version": {
                    "type": "object",
                    "required": ["requestId", "applicationVersion", "protocolVersion", "controlSchemaVersion", "nodeId"],
                    "properties": {
                        "requestId": { "type": "string" },
                        "applicationVersion": { "type": "string" },
                        "protocolVersion": { "$ref": "#/components/schemas/ProtocolVersion" },
                        "controlSchemaVersion": { "type": "integer", "minimum": 0 },
                        "nodeId": { "type": "string" }
                    }
                },
                "ReadinessCheck": {
                    "type": "object",
                    "required": ["name", "status"],
                    "properties": {
                        "name": { "type": "string" },
                        "status": { "type": "string", "enum": ["ok", "warning", "error"] },
                        "code": { "type": "string" }
                    }
                },
                "Readiness": {
                    "type": "object",
                    "required": ["requestId", "status", "checks"],
                    "properties": {
                        "requestId": { "type": "string" },
                        "status": { "type": "string", "enum": ["ready", "degraded", "not_ready"] },
                        "checks": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/ReadinessCheck" }
                        }
                    }
                },
                "SessionInfo": {
                    "type": "object",
                    "required": ["sessionId", "displayPrefix", "hostname", "createdAt", "lastUsedAt", "expiresAt", "revoked"],
                    "properties": {
                        "sessionId": { "type": "string" },
                        "displayPrefix": { "type": "string" },
                        "hostname": { "type": "string" },
                        "createdAt": { "type": "string", "format": "date-time" },
                        "lastUsedAt": { "type": "string", "format": "date-time" },
                        "expiresAt": { "type": "string", "format": "date-time" },
                        "revoked": { "type": "boolean" }
                    }
                },
                "SessionCreated": {
                    "type": "object",
                    "required": ["requestId", "sessionId", "displayPrefix", "expiresAt"],
                    "properties": {
                        "requestId": { "type": "string" },
                        "sessionId": { "type": "string" },
                        "displayPrefix": { "type": "string" },
                        "expiresAt": { "type": "string", "format": "date-time" }
                    }
                },
                "Sessions": {
                    "type": "object",
                    "required": ["requestId", "sessions"],
                    "properties": {
                        "requestId": { "type": "string" },
                        "sessions": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/SessionInfo" }
                        }
                    }
                },
                "LibrarySort": { "type": "string", "enum": ["createdDesc", "filenameAsc", "sizeDesc"] },
                "GlobalMediaVerificationState": { "type": "string", "enum": ["unverified", "provisional", "verified", "mismatch"] },
                "NodeAvailabilityState": {
                    "type": "string",
                    "enum": [
                        "known",
                        "accessible_here",
                        "accessible_on_another_node",
                        "temporarily_unavailable",
                        "missing_after_completed_observation",
                        "fingerprint_mismatch"
                    ]
                },
                "MediaObject": {
                    "type": "object",
                    "required": ["id", "sizeBytes", "quickFingerprint", "fullContentHash", "verificationState", "createdAt", "updatedAt"],
                    "properties": {
                        "id": { "type": "string" },
                        "sizeBytes": { "type": ["integer", "null"], "minimum": 0 },
                        "quickFingerprint": { "type": ["string", "null"] },
                        "fullContentHash": { "type": ["string", "null"] },
                        "verificationState": { "$ref": "#/components/schemas/GlobalMediaVerificationState" },
                        "createdAt": { "type": "string", "format": "date-time" },
                        "updatedAt": { "type": "string", "format": "date-time" }
                    }
                },
                "LibrarySearchItem": {
                    "type": "object",
                    "required": ["media", "filename", "cameraMake", "cameraModel", "primaryStorageRootId", "storageLocationCount", "projectMembershipCount", "availability", "thumbnailRepresentationId"],
                    "properties": {
                        "media": { "$ref": "#/components/schemas/MediaObject" },
                        "filename": { "type": "string" },
                        "cameraMake": { "type": ["string", "null"] },
                        "cameraModel": { "type": ["string", "null"] },
                        "primaryStorageRootId": { "type": ["string", "null"] },
                        "storageLocationCount": { "type": "integer", "minimum": 0 },
                        "projectMembershipCount": { "type": "integer", "minimum": 0 },
                        "availability": { "$ref": "#/components/schemas/NodeAvailabilityState" },
                        "thumbnailRepresentationId": { "type": ["string", "null"] }
                    }
                },
                "LibrarySearchPage": {
                    "type": "object",
                    "required": ["items", "nextCursor", "availabilityFilterApplied"],
                    "properties": {
                        "items": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/LibrarySearchItem" }
                        },
                        "nextCursor": { "type": ["string", "null"] },
                        "availabilityFilterApplied": { "type": "boolean" }
                    }
                },
                "MediaSearchFacts": {
                    "type": "object",
                    "required": ["filename", "cameraMake", "cameraModel", "primaryStorageRootId", "storageLocationCount", "projectMembershipCount"],
                    "properties": {
                        "filename": { "type": "string" },
                        "cameraMake": { "type": ["string", "null"] },
                        "cameraModel": { "type": ["string", "null"] },
                        "primaryStorageRootId": { "type": ["string", "null"] },
                        "storageLocationCount": { "type": "integer", "minimum": 0 },
                        "projectMembershipCount": { "type": "integer", "minimum": 0 }
                    }
                },
                "StorageLocationAvailability": {
                    "type": "object",
                    "required": ["nodeId", "storageObjectId", "state", "completedGenerationId", "completedGenerationSequence"],
                    "properties": {
                        "nodeId": { "type": "string" },
                        "storageObjectId": { "type": "string" },
                        "state": { "$ref": "#/components/schemas/NodeAvailabilityState" },
                        "completedGenerationId": { "type": ["string", "null"] },
                        "completedGenerationSequence": { "type": ["integer", "null"], "minimum": 0 }
                    }
                },
                "MediaAvailability": {
                    "type": "object",
                    "required": ["mediaObjectId", "queriedNodeId", "state", "locations"],
                    "properties": {
                        "mediaObjectId": { "type": "string" },
                        "queriedNodeId": { "type": "string" },
                        "state": { "$ref": "#/components/schemas/NodeAvailabilityState" },
                        "locations": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/StorageLocationAvailability" }
                        }
                    }
                },
                "LibraryMediaDetail": {
                    "type": "object",
                    "required": ["media", "facts", "availability"],
                    "properties": {
                        "media": { "$ref": "#/components/schemas/MediaObject" },
                        "facts": { "$ref": "#/components/schemas/MediaSearchFacts" },
                        "availability": { "$ref": "#/components/schemas/MediaAvailability" }
                    }
                },
                "ProjectSummary": {
                    "type": "object",
                    "required": ["projectId", "state", "assetCount", "registeredAt", "updatedAt"],
                    "properties": {
                        "projectId": { "type": "string" },
                        "state": { "type": "string" },
                        "assetCount": { "type": "integer", "minimum": 0 },
                        "registeredAt": { "type": "string", "format": "date-time" },
                        "updatedAt": { "type": "string", "format": "date-time" }
                    }
                },
                "ProjectPage": {
                    "type": "object",
                    "required": ["items", "nextCursor"],
                    "properties": {
                        "items": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/ProjectSummary" }
                        },
                        "nextCursor": { "type": ["string", "null"] }
                    }
                },
                "ProjectAssetItem": {
                    "type": "object",
                    "required": ["projectAssetId", "filename", "camera", "mediaObjectId", "mediaVerificationState", "removed", "rotationDegrees", "labels", "activeNote", "durationSeconds", "bestTime"],
                    "properties": {
                        "projectAssetId": { "type": "string" },
                        "filename": { "type": "string" },
                        "camera": { "type": "string" },
                        "mediaObjectId": { "type": ["string", "null"] },
                        "mediaVerificationState": { "type": "string" },
                        "removed": { "type": "boolean" },
                        "rotationDegrees": { "type": "integer" },
                        "labels": { "type": "array", "items": { "type": "string" } },
                        "activeNote": { "type": ["string", "null"] },
                        "durationSeconds": { "type": "number" },
                        "bestTime": { "type": ["string", "null"], "format": "date-time" }
                    }
                },
                "ProjectAssetPage": {
                    "type": "object",
                    "required": ["items", "nextCursor"],
                    "properties": {
                        "items": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/ProjectAssetItem" }
                        },
                        "nextCursor": { "type": ["string", "null"] }
                    }
                },
                "AddToProjectRequest": {
                    "type": "object",
                    "required": ["mediaObjectIds", "acknowledgeUnverified", "idempotencyKey"],
                    "properties": {
                        "mediaObjectIds": { "type": "array", "minItems": 1, "maxItems": 100, "items": { "type": "string" } },
                        "acknowledgeUnverified": { "type": "boolean" },
                        "idempotencyKey": { "type": "string", "minLength": 1, "maxLength": 255 }
                    }
                },
                "AddToProjectDisposition": {
                    "type": "string",
                    "enum": ["ready", "added", "already_member", "acknowledgement_required", "identity_mismatch"]
                },
                "AddToProjectItem": {
                    "type": "object",
                    "required": ["mediaObjectId", "projectAssetId", "verificationState", "disposition", "remoteOnly"],
                    "properties": {
                        "mediaObjectId": { "type": "string" },
                        "projectAssetId": { "type": ["string", "null"] },
                        "verificationState": { "$ref": "#/components/schemas/GlobalMediaVerificationState" },
                        "disposition": { "$ref": "#/components/schemas/AddToProjectDisposition" },
                        "remoteOnly": { "type": "boolean" }
                    }
                },
                "AddToProjectPreview": {
                    "type": "object",
                    "required": ["projectId", "selectedCount", "readyCount", "alreadyMemberCount", "acknowledgementRequiredCount", "mismatchCount", "items"],
                    "properties": {
                        "projectId": { "type": "string" },
                        "selectedCount": { "type": "integer", "minimum": 0 },
                        "readyCount": { "type": "integer", "minimum": 0 },
                        "alreadyMemberCount": { "type": "integer", "minimum": 0 },
                        "acknowledgementRequiredCount": { "type": "integer", "minimum": 0 },
                        "mismatchCount": { "type": "integer", "minimum": 0 },
                        "items": { "type": "array", "items": { "$ref": "#/components/schemas/AddToProjectItem" } }
                    }
                },
                "AddToProjectResult": {
                    "type": "object",
                    "required": ["projectId", "idempotencyKey", "addedCount", "alreadyMemberCount", "blockedCount", "items"],
                    "properties": {
                        "projectId": { "type": "string" },
                        "idempotencyKey": { "type": "string" },
                        "addedCount": { "type": "integer", "minimum": 0 },
                        "alreadyMemberCount": { "type": "integer", "minimum": 0 },
                        "blockedCount": { "type": "integer", "minimum": 0 },
                        "items": { "type": "array", "items": { "$ref": "#/components/schemas/AddToProjectItem" } }
                    }
                },
                "ReviewRepresentation": {
                    "type": "object",
                    "required": ["representationId", "state", "availableOnServer"],
                    "properties": {
                        "representationId": { "type": "string" },
                        "state": { "type": "string" },
                        "availableOnServer": { "type": "boolean" }
                    }
                },
                "ProjectReviewAsset": {
                    "allOf": [
                        { "$ref": "#/components/schemas/ProjectAssetItem" },
                        { "type": "object", "required": ["materializedRevision", "activeNoteRevisionId", "noteRevisionCount", "proxy", "thumbnail", "originalAvailability", "unresolvedConflictCount"], "properties": {
                            "materializedRevision": { "type": "integer", "minimum": 0 },
                            "activeNoteRevisionId": { "type": ["string", "null"] },
                            "noteRevisionCount": { "type": "integer", "minimum": 0 },
                            "proxy": { "oneOf": [{ "$ref": "#/components/schemas/ReviewRepresentation" }, { "type": "null" }] },
                            "thumbnail": { "oneOf": [{ "$ref": "#/components/schemas/ReviewRepresentation" }, { "type": "null" }] },
                            "originalAvailability": { "oneOf": [{ "$ref": "#/components/schemas/NodeAvailabilityState" }, { "type": "null" }] },
                            "unresolvedConflictCount": { "type": "integer", "minimum": 0 }
                        } }
                    ]
                },
                "ReviewMutationResult": {
                    "type": "object",
                    "required": ["asset", "staleBaseRevision"],
                    "properties": {
                        "asset": { "$ref": "#/components/schemas/ProjectReviewAsset" },
                        "staleBaseRevision": { "type": "boolean" }
                    }
                },
                "NoteMutationRequest": { "type": "object", "required": ["text", "baseRevision"], "properties": { "text": { "type": "string", "maxLength": 100000 }, "baseRevision": { "type": "integer", "minimum": 0 } } },
                "LabelMutationRequest": { "type": "object", "required": ["label", "baseRevision"], "properties": { "label": { "type": "string" }, "baseRevision": { "type": "integer", "minimum": 0 } } },
                "RemovedMutationRequest": { "type": "object", "required": ["removed", "baseRevision"], "properties": { "removed": { "type": "boolean" }, "baseRevision": { "type": "integer", "minimum": 0 } } },
                "RotationMutationRequest": { "type": "object", "required": ["rotationDegrees", "baseRevision"], "properties": { "rotationDegrees": { "type": "integer", "enum": [0, 90, 180, 270] }, "baseRevision": { "type": "integer", "minimum": 0 } } }
            },
            "requestBodies": {
                "AdminPayload": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/AdminPayload" } } } }
            },
            "responses": {
                "AdminPayload": { "description": "Authenticated administrative projection or canonical mutation result", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/AdminPayload" } } } },
                "Version": { "description": "Server identity and protocol", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Version" } } } },
                "Readiness": { "description": "Readiness report", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Readiness" } } } },
                "SessionCreated": { "description": "Browser session created", "headers": { "x-csrf-token": { "schema": { "type": "string" } } }, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/SessionCreated" } } } },
                "Sessions": { "description": "Browser sessions", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Sessions" } } } },
                "LibrarySearchPage": { "description": "A page of the global media library search", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/LibrarySearchPage" } } } },
                "LibraryMediaDetail": { "description": "One media object with its search facts and node-relative availability", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/LibraryMediaDetail" } } } },
                "ProjectPage": { "description": "A page of registered project replicas", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ProjectPage" } } } },
                "ProjectAssetPage": { "description": "A page of one project's assets", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ProjectAssetPage" } } } },
                "AddToProjectPreview": { "description": "A non-mutating preview of add-to-project outcomes", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/AddToProjectPreview" } } } },
                "AddToProjectResult": { "description": "Canonical add-to-project outcomes", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/AddToProjectResult" } } } },
                "ProjectReviewAsset": { "description": "Canonical project asset review projection", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ProjectReviewAsset" } } } },
                "ReviewMutationResult": { "description": "Canonical asset after an operation-backed review mutation", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ReviewMutationResult" } } } },
                "Error": { "description": "Stable server error", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Error" } } } }
            }
        }
    })
}

pub(super) async fn openapi() -> Json<serde_json::Value> {
    Json(openapi_document())
}
