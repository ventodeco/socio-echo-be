use actix_web::{get, HttpRequest, HttpResponse};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use crate::utils::validate_token;

#[derive(Debug, Serialize)]
pub struct DashboardCityCountResponse {
    pub success: bool,
    pub data: Option<DashboardCityCountData>,
    pub errors: Option<Vec<DashboardError>>,
}

#[derive(Debug, Serialize)]
pub struct DashboardCityCountData {
    pub cities: HashMap<String, i64>,
}

#[derive(Debug, Serialize)]
pub struct DashboardError {
    pub code: String,
    pub entity: String,
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub struct DashboardCityCountQuery {
    pub cities: String, // comma separated
}

#[get("/summary/city")]
pub async fn get_city_count(
    req: HttpRequest,
    query: actix_web::web::Query<std::collections::HashMap<String, String>>,
) -> HttpResponse {
    // JWT validation
    let jwt_secret = std::env::var("JWT_SECRET").unwrap_or_default();
    let token = req.headers().get("x-user-token").and_then(|v| v.to_str().ok());
    if token.is_none() {
        return HttpResponse::Unauthorized().json(DashboardCityCountResponse {
            success: false,
            data: None,
            errors: Some(vec![DashboardError {
                code: "401".to_string(),
                entity: "SOCIO_ECHO".to_string(),
                message: "MISSING_OR_INVALID_TOKEN".to_string(),
            }]),
        });
    }
    if validate_token(token.unwrap(), &jwt_secret).is_err() {
        return HttpResponse::Unauthorized().json(DashboardCityCountResponse {
            success: false,
            data: None,
            errors: Some(vec![DashboardError {
                code: "401".to_string(),
                entity: "SOCIO_ECHO".to_string(),
                message: "MISSING_OR_INVALID_TOKEN".to_string(),
            }]),
        });
    }

    // Check for 'cities' param
    let cities_param = query.get("cities");
    if cities_param.is_none() {
        return HttpResponse::BadRequest().json(DashboardCityCountResponse {
            success: false,
            data: None,
            errors: Some(vec![DashboardError {
                code: "1003".to_string(),
                entity: "SOCIO_ECHO".to_string(),
                message: "Query param 'cities' is required".to_string(),
            }]),
        });
    }
    let city_list: Vec<String> = cities_param.unwrap().split(',').map(|c| c.trim().to_string()).collect();

    // Prepare ElasticSearch query

    // ELASTICSEARCH_URL="https://100.95.170.73:9200"
    // ELASTICSEARCH_USER='elastic'
    // ELASTICSEARCH_PASS='wWEVEfUXVVnIyz8=a62k'
    let es_url = "https://100.95.170.73:9200";
    let es_user = "elastic";
    let es_pass = "wWEVEfUXVVnIyz8=a62k";
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();

    let es_url = format!("{}/media-online-*/_search?pretty", es_url.clone());
    let es_body = serde_json::json!({
        "size": 0,
        "query": {
            "bool": {
                "filter": [
                    { "range": { "published_at": { "gte": "now-100w/w", "lt": "now/w" } } }
                ]
            }
        },
        "aggs": {
            "cities_count": {
                "terms": {
                    "field": "cities.keyword",
                    "include": city_list,
                    "size": 10
                }
            }
        }
    });

    let res = client
        .get(&es_url)
        .basic_auth(es_user.clone(), Some(es_pass.clone()))
        .header("Content-Type", "application/json")
        .json(&es_body)
        .send()
        .await;

    let res_json = match res {
        Ok(r) => r,
        Err(e) => {
            return HttpResponse::InternalServerError().json(DashboardCityCountResponse {
                success: false,
                data: None,
                errors: Some(vec![DashboardError {
                    code: "100".to_string(),
                    entity: "SOCIO_ECHO".to_string(),
                    message: format!("ELASTIC_REQUEST_ERROR: {}", e),
                }]),
            });
        }
    };

    log::info!("tes: {:?}", res_json);

    let val = match res_json.json::<serde_json::Value>().await {
        Ok(val) => val,
        Err(e) => {
            return HttpResponse::InternalServerError().json(DashboardCityCountResponse {
                success: false,
                data: None,
                errors: Some(vec![DashboardError {
                    code: "100".to_string(),
                    entity: "SOCIO_ECHO".to_string(),
                    message: format!("ELASTIC_PARSE_ERROR: {}", e),
                }]),
            });
        }
    };
    let mut cities = HashMap::new();
    if let Some(buckets) = val["aggregations"]["cities_count"]["buckets"].as_array() {
        for bucket in buckets {
            if let (Some(key), Some(doc_count)) = (bucket["key"].as_str(), bucket["doc_count"].as_i64()) {
                cities.insert(key.to_string(), doc_count);
            }
        }
    }
    let mut response_cities = HashMap::new();
    for city in &city_list {
        response_cities.insert(city.clone(), *cities.get(city).unwrap_or(&0));
    }
    let data = DashboardCityCountData { cities: response_cities };
    HttpResponse::Ok().json(DashboardCityCountResponse {
        success: true,
        data: Some(data),
        errors: None,
    })
} 