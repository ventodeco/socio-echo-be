use std::{collections::HashMap, time::Duration};
use uuid::Uuid;
use serde_json::json;
use base64::{Engine as _, engine::general_purpose::STANDARD};

use crate::{
    commons::minio_service::{self, MinioService},
    models::user::ApiError,
    services::{face_match_service::FaceMatchService, metrics_service::MetricsService},
    submissions::{
        dto::presigned_urls_response::{Document, PresignedUrlsResponse, SubmissionData}, 
        submission_controller::{GetSubmissionStatusResponse, ProcessSubmissionResponse, SubmissionType}, 
        submission_repository::SubmissionRepository
    },
};

pub struct SubmissionService {
    minio_service: MinioService,
    submission_repository: SubmissionRepository,
    metrics: MetricsService,
}

impl SubmissionService {
    pub fn new(
        minio_service: MinioService, 
        submission_repository: SubmissionRepository, 
        metrics: MetricsService
    ) -> Self {
        Self {
            minio_service,
            submission_repository,
            metrics,
        }
    }

    pub async fn generate_presigned_urls(
        &self,
        session_id: String,
        user_id: String,
        submission_type: SubmissionType,
        nfc_identifier: String,
    ) -> Result<PresignedUrlsResponse, Vec<ApiError>> {
        let start = std::time::Instant::now();
        let mut tags = HashMap::new();
        tags.insert("endpoint".to_string(), "presigned_urls".to_string());
        tags.insert("submission_type".to_string(), submission_type.to_string());

        // Generate a new submission ID
        let submission_id = Uuid::new_v4();

        // Generate document references and presigned URLs
        let mut documents = HashMap::new();

        let mut documents_data = HashMap::new();

        // KYC document
        if submission_type.to_string() == "KYC" {
            let ktp_uuid = Uuid::new_v4();
            let ktp_filename = ktp_uuid.to_string() + "_KTP";
            let ktp_url = match self.minio_service
                .generate_upload_url(ktp_filename.clone(), Duration::from_secs(600))
                .await
            {
                Ok(url) => url,
                Err(e) => {
                    self.metrics.increment("api_error", Some(tags.clone()));
                    return Err(vec![ApiError {
                        entity: "SOCIO_ECHO_BE".to_string(),
                        code: "1001".to_string(),
                        cause: e.to_string(),
                    }]);
                }
            };

            documents.insert(
                "KTP".to_string(),
                Document {
                    document_url: ktp_url,
                    document_reference: ktp_uuid.to_string(),
                    expiry_in_seconds: "600".to_string(),
                },
            );

            documents_data.insert("KTP", SubmissionData {
                document_name: ktp_filename.clone(),
                document_reference: ktp_uuid.to_string(),
            });
        }

        // Selfie document
        let selfie_uuid: Uuid = Uuid::new_v4();
        let selfie_filename = selfie_uuid.to_string() + "_SELFIE";
        let selfie_url = match self.minio_service
            .generate_upload_url(selfie_filename.clone(), Duration::from_secs(600))
            .await
        {
            Ok(url) => url,
            Err(e) => {
                self.metrics.increment("api_error", Some(tags.clone()));
                return Err(vec![ApiError {
                    entity: "SOCIO_ECHO_BE".to_string(),
                    code: "1001".to_string(),
                    cause: e.to_string(),
                }]);
            }
        };

        documents.insert(
            "SELFIE".to_string(),
            Document {
                document_url: selfie_url,
                document_reference: selfie_uuid.to_string(),
                expiry_in_seconds: "600".to_string(),
            },
        );
        documents_data.insert("SELFIE", SubmissionData {
            document_name: selfie_filename.clone(),
            document_reference: selfie_uuid.to_string()
        });

        // NFC document
        let nfc_identifier_clean = nfc_identifier.replace("data:image/jpeg;base64,", "");
        let nfc_identifier_base64 = STANDARD.decode(&nfc_identifier_clean).unwrap();
        let nfc_uuid = Uuid::new_v4();
        let nfc_identifier_filename = nfc_uuid.to_string() + "_NFC";
        self.minio_service.upload_file(nfc_identifier_filename.clone(), nfc_identifier_base64, Some("image/jpeg".to_string())).await.unwrap();
        documents_data.insert("NFC", SubmissionData {
            document_name: nfc_identifier_filename.clone(),
            document_reference: nfc_uuid.to_string(),
        });

        let response = PresignedUrlsResponse {
            submission_id: submission_id.to_string(),
            documents,
        };

        // Save to database
        if let Err(e) = self
            .submission_repository
            .create(
                submission_id,
                &format!("{:?}", submission_type),
                &session_id,
                &user_id,
                "INITIATED",
                json!(documents_data),
                json!({}),
                nfc_identifier_clean.clone().chars().take(500).collect::<String>(),
            )
            .await
        {
            self.metrics.increment("api_error", Some(tags.clone()));
            return Err(vec![ApiError {
                entity: "SOCIO_ECHO_BE".to_string(),
                code: "1002".to_string(),
                cause: e.to_string(),
            }]);
        }

        self.metrics.increment("api_success", Some(tags.clone()));
        self.metrics.timing("api_latency", start.elapsed(), Some(tags));

        Ok(response)
    }

    pub async fn process_submission(
        &self,
        submission_id: String,
        face_match_service: FaceMatchService,
    ) -> Result<ProcessSubmissionResponse, Vec<ApiError>> {
        let start = std::time::Instant::now();
        let mut tags = HashMap::new();
        tags.insert("endpoint".to_string(), "process_submission".to_string());

        // 1. Check if submission exists in database
        let (submission_type, nfc_identifier, submission_data) = match self.submission_repository.find_submission_by_id(&submission_id).await {
            Ok(Some((submission_type, nfc_identifier, data))) => (submission_type, nfc_identifier, data),
            Ok(None) => {
                self.metrics.increment("process_submission.error", Some(tags.clone()));
                self.metrics.timing("process_submission.duration", start.elapsed(), Some(tags));
                return Err(vec![ApiError {
                    entity: "SOCIO_ECHO_BE".to_string(),
                    code: "1004".to_string(),
                    cause: "SUBMISSION_NOT_FOUND".to_string(),
                }]);
            }
            Err(e) => {
                self.metrics.increment("process_submission.error", Some(tags.clone()));
                self.metrics.timing("process_submission.duration", start.elapsed(), Some(tags));
                return Err(vec![ApiError {
                    entity: "SOCIO_ECHO_BE".to_string(),
                    code: "1002".to_string(),
                    cause: e.to_string(),
                }]);
            }
        };


        let mut image_url_1 = String::new();
        let mut image_url_2 = String::new();

        // 2. Extract document names from submission data
        let documents_data = match submission_data.as_object() {
            Some(obj) => obj,
            None => {
                self.metrics.increment("process_submission.error", Some(tags.clone()));
                self.metrics.timing("process_submission.duration", start.elapsed(), Some(tags));
                return Err(vec![ApiError {
                    entity: "SOCIO_ECHO_BE".to_string(),
                    code: "1004".to_string(),
                    cause: "INVALID_SUBMISSION_DATA".to_string(),
                }]);
            }
        };

        // 3. Get selfie document name
        let selfie_doc = match documents_data.get("SELFIE") {
            Some(doc) => doc,
            None => {
                self.metrics.increment("process_submission.error", Some(tags.clone()));
                self.metrics.timing("process_submission.duration", start.elapsed(), Some(tags));
                return Err(vec![ApiError {
                    entity: "SOCIO_ECHO_BE".to_string(),
                    code: "1004".to_string(),
                    cause: "SELFIE_DOES_NOT_EXIST".to_string(),
                }]);
            }
        };

        let selfie_filename = match selfie_doc.get("documentName") {
            Some(name) => name.as_str().unwrap_or(""),
            None => {
                self.metrics.increment("process_submission.error", Some(tags.clone()));
                self.metrics.timing("process_submission.duration", start.elapsed(), Some(tags));
                return Err(vec![ApiError {
                    entity: "SOCIO_ECHO_BE".to_string(),
                    code: "1004".to_string(),
                    cause: "SELFIE_DOES_NOT_EXIST".to_string(),
                }]);
            }
        };

        // 4. Check if selfie exists in MinIO
        if !self.minio_service.file_exists(selfie_filename.to_string()).await.unwrap_or(false) {
            self.metrics.increment("process_submission.error", Some(tags.clone()));
            self.metrics.timing("process_submission.duration", start.elapsed(), Some(tags));
            return Err(vec![ApiError {
                entity: "SOCIO_ECHO_BE".to_string(),
                code: "1004".to_string(),
                cause: "SELFIE_DOES_NOT_EXIST".to_string(),
            }]);
        }

        // 6. Generate URLs for face matching
        let selfie_url = match self.minio_service.generate_view_url(selfie_filename.to_string()).await {
            Ok(url) => url,
            Err(e) => {
                self.metrics.increment("process_submission.error", Some(tags.clone()));
                self.metrics.timing("process_submission.duration", start.elapsed(), Some(tags));
                return Err(vec![ApiError {
                    entity: "SOCIO_ECHO_BE".to_string(),
                    code: "1001".to_string(),
                    cause: e.to_string(),
                }]);
            }
        };

        log::info!("selfie_url: {:?}", selfie_url);

        if submission_type == "KYC" {

            // 5. Get NFC document name
            let nfc_doc = match documents_data.get("NFC") {
                Some(doc) => doc,
                None => {
                    self.metrics.increment("process_submission.error", Some(tags.clone()));
                    self.metrics.timing("process_submission.duration", start.elapsed(), Some(tags));
                    return Err(vec![ApiError {
                        entity: "SOCIO_ECHO_BE".to_string(),
                        code: "1004".to_string(),
                        cause: "NFC_DOES_NOT_EXIST".to_string(),
                    }]);
                }
            };

            let nfc_filename = match nfc_doc.get("documentName") {
                Some(name) => name.as_str().unwrap_or(""),
                None => {
                    self.metrics.increment("process_submission.error", Some(tags.clone()));
                    self.metrics.timing("process_submission.duration", start.elapsed(), Some(tags));
                    return Err(vec![ApiError {
                        entity: "SOCIO_ECHO_BE".to_string(),
                        code: "1004".to_string(),
                        cause: "NFC_DOES_NOT_EXIST".to_string(),
                    }]);
                }
            };

            let nfc_url = match self.minio_service.generate_view_url(nfc_filename.to_string()).await {
                Ok(url) => url,
                Err(e) => {
                    self.metrics.increment("process_submission.error", Some(tags.clone()));
                    self.metrics.timing("process_submission.duration", start.elapsed(), Some(tags));
                    return Err(vec![ApiError {
                        entity: "SOCIO_ECHO_BE".to_string(),
                        code: "1001".to_string(),
                        cause: e.to_string(),
                    }]);
                }
            };

            log::info!("nfc_url: {:?}", nfc_url);

            image_url_1 = nfc_url;
            image_url_2 = selfie_url;

        } else if submission_type == "ON_DEMAND" {

            // 1. Check if submission exists in database
            let submission_data_existing = match self.submission_repository.find_submission_by_nfc_identifier_and_status(&nfc_identifier, "APPROVED").await {
                Ok(Some(submission_data_existing)) => submission_data_existing,
                Ok(None) => {
                    self.metrics.increment("process_submission.error", Some(tags.clone()));
                    self.metrics.timing("process_submission.duration", start.elapsed(), Some(tags));
                    return Err(vec![ApiError {
                        entity: "SOCIO_ECHO_BE".to_string(),
                        code: "1004".to_string(),
                        cause: "SUBMISSION_NOT_FOUND".to_string(),
                    }]);
                }
                Err(e) => {
                    self.metrics.increment("process_submission.error", Some(tags.clone()));
                    self.metrics.timing("process_submission.duration", start.elapsed(), Some(tags));
                    return Err(vec![ApiError {
                        entity: "SOCIO_ECHO_BE".to_string(),
                        code: "1002".to_string(),
                        cause: e.to_string(),
                    }]);
                }
            };

            // 2. Extract document names from submission data
            let documents_data_existing = match submission_data_existing.as_object() {
                Some(obj) => obj,
                None => {
                    self.metrics.increment("process_submission.error", Some(tags.clone()));
                    self.metrics.timing("process_submission.duration", start.elapsed(), Some(tags));
                    return Err(vec![ApiError {
                        entity: "SOCIO_ECHO_BE".to_string(),
                        code: "1004".to_string(),
                        cause: "INVALID_SUBMISSION_DATA".to_string(),
                    }]);
                }
            };

            // 3. Get selfie document name
            let selfie_doc_existing = match documents_data_existing.get("SELFIE") {
                Some(doc) => doc,
                None => {
                    self.metrics.increment("process_submission.error", Some(tags.clone()));
                    self.metrics.timing("process_submission.duration", start.elapsed(), Some(tags));
                    return Err(vec![ApiError {
                        entity: "SOCIO_ECHO_BE".to_string(),
                        code: "1004".to_string(),
                        cause: "SELFIE_DOES_NOT_EXIST".to_string(),
                    }]);
                }
            };

            let selfie_filename_existing = match selfie_doc_existing.get("documentName") {
                Some(name) => name.as_str().unwrap_or(""),
                None => {
                    self.metrics.increment("process_submission.error", Some(tags.clone()));
                    self.metrics.timing("process_submission.duration", start.elapsed(), Some(tags));
                    return Err(vec![ApiError {
                        entity: "SOCIO_ECHO_BE".to_string(),
                        code: "1004".to_string(),
                        cause: "SELFIE_DOES_NOT_EXIST".to_string(),
                    }]);
                }
            };

            // 4. Check if selfie exists in MinIO
            if !self.minio_service.file_exists(selfie_filename_existing.to_string()).await.unwrap_or(false) {
                self.metrics.increment("process_submission.error", Some(tags.clone()));
                self.metrics.timing("process_submission.duration", start.elapsed(), Some(tags));
                return Err(vec![ApiError {
                    entity: "SOCIO_ECHO_BE".to_string(),
                    code: "1004".to_string(),
                    cause: "SELFIE_DOES_NOT_EXIST".to_string(),
                }]);
            }

            // 6. Generate URLs for face matching
            let selfie_url_existing = match self.minio_service.generate_view_url(selfie_filename_existing.to_string()).await {
                Ok(url) => url,
                Err(e) => {
                    self.metrics.increment("process_submission.error", Some(tags.clone()));
                    self.metrics.timing("process_submission.duration", start.elapsed(), Some(tags));
                    return Err(vec![ApiError {
                        entity: "SOCIO_ECHO_BE".to_string(),
                        code: "1001".to_string(),
                        cause: e.to_string(),
                    }]);
                }
            };

            log::info!("selfie_url_existing: {:?}", selfie_url_existing);

            image_url_1 = selfie_url_existing;
            image_url_2 = selfie_url;

        } else {
            return Err(vec![ApiError {
                entity: "SOCIO_ECHO_BE".to_string(),
                code: "1004".to_string(),
                cause: "INVALID_SUBMISSION_TYPE".to_string(),
            }]);
        }

        // 7. Perform face matching
        let face_match_result = match face_match_service.compare_faces(
            image_url_1,
            image_url_2,
            submission_id.clone(),
        ).await {
            Ok(result) => result,
            Err(e) => {
                self.metrics.increment("process_submission.error", Some(tags.clone()));
                self.metrics.timing("process_submission.duration", start.elapsed(), Some(tags));
                return Err(vec![ApiError {
                    entity: "SOCIO_ECHO_BE".to_string(),
                    code: "1006".to_string(),
                    cause: e.to_string(),
                }]);
            }
        };

        // 8. Update submission status based on face match result
        let new_status = if face_match_result.is_match { "APPROVED" } else { "REJECTED" };
        
        if let Err(e) = self.submission_repository.update_submission_status(&submission_id, new_status).await {
            self.metrics.increment("process_submission.error", Some(tags.clone()));
            self.metrics.timing("process_submission.duration", start.elapsed(), Some(tags));
            return Err(vec![ApiError {
                entity: "SOCIO_ECHO_BE".to_string(),
                code: "1002".to_string(),
                cause: e.to_string(),
            }]);
        }

        // 9. Return response
        let response = ProcessSubmissionResponse {
            submission_status: new_status.to_string(),
        };

        self.metrics.increment("process_submission.success", Some(tags.clone()));
        self.metrics.timing("process_submission.duration", start.elapsed(), Some(tags));

        Ok(response)
    }

    pub async fn get_submission_status(
        &self,
        submission_type: SubmissionType,
        nfc_identifier: String,
    ) -> Result<GetSubmissionStatusResponse, Vec<ApiError>> {
        let submission_data= match self.submission_repository.find_submission_by_nfc_identifier_and_submission_type(&submission_type.to_string(), &nfc_identifier.chars().take(500).collect::<String>()).await {
            Ok(Some(status)) => status,
            Ok(None) => {
                return Err(vec![ApiError {
                    entity: "SOCIO_ECHO_BE".to_string(),
                    code: "1004".to_string(),
                    cause: "SUBMISSION_NOT_FOUND".to_string(),
                }]);
            }
            Err(e) => {
                return Err(vec![ApiError {
                    entity: "SOCIO_ECHO_BE".to_string(),
                    code: "1002".to_string(),
                    cause: e.to_string(),
                }]);
            }
        };

        let mut status: String = String::from("NOT_KYC");
        if submission_data == "APPROVED" {
            status = String::from("KYC");
        }

        return Ok(GetSubmissionStatusResponse {
            submission_status: status,
        });
    }

}
