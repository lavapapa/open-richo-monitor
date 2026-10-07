use crate::availability::{evaluate_availability, Availability, AvailabilityError};
use serde_json::value::RawValue;
use serde_json::{Map, Number, Value};
use std::{collections::BTreeSet, fmt};

pub const MAX_RESPONSE_BYTES: usize = 1_048_576;
pub const MAX_PRODUCT_NAME_BYTES: usize = 1024;
pub const MAX_PRODUCT_PAGE_ITEMS: usize = 20;
const MAX_METADATA_TEXT_BYTES: usize = 256;
const MAX_IMAGE_URL_BYTES: usize = 2048;
const MAX_PRICE_TEXT_BYTES: usize = 64;
const MAX_GALLERY_IMAGES: usize = 8;

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProductMetadata {
    pub image_url: Option<String>,
    pub gallery_urls: Vec<String>,
    pub price: Option<String>,
    pub unit_name: Option<String>,
    pub product_no: Option<String>,
    pub is_member_card: bool,
    pub member_card_days: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProductDetail {
    pub product_id: u64,
    pub name: String,
    pub is_show: u8,
    pub stock: Number,
    pub availability: Availability,
    #[serde(default)]
    pub metadata: ProductMetadata,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RicohParseError {
    InvalidRequestedProductId,
    ResponseTooLarge {
        bytes: usize,
    },
    InvalidJson,
    MissingField(&'static str),
    InvalidType {
        field: &'static str,
        expected: &'static str,
        actual: &'static str,
    },
    InvalidValue {
        field: &'static str,
        expected: &'static str,
    },
    BusinessCode {
        code: i64,
    },
    ProductIdMismatch {
        requested: u64,
        response: u64,
    },
}

impl fmt::Display for RicohParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequestedProductId => formatter.write_str("请求的 Product ID 无效"),
            Self::ResponseTooLarge { bytes } => write!(formatter, "响应大小为 {bytes} 字节"),
            Self::InvalidJson => formatter.write_str("响应不是有效 JSON"),
            Self::MissingField(field) => write!(formatter, "响应缺少字段 {field}"),
            Self::InvalidType {
                field,
                expected,
                actual,
            } => write!(
                formatter,
                "字段 {field} 类型错误：预期 {expected}，实际为 {actual}"
            ),
            Self::InvalidValue { field, expected } => {
                write!(formatter, "字段 {field} 的值无效：预期 {expected}")
            }
            Self::BusinessCode { code } => write!(formatter, "接口返回业务错误码 {code}"),
            Self::ProductIdMismatch {
                requested,
                response,
            } => write!(
                formatter,
                "响应商品编号 {response} 与请求编号 {requested} 不一致"
            ),
        }
    }
}

impl std::error::Error for RicohParseError {}

/// `response` 是解压后的 JSON 响应字节，大小上限按该字节数计算。
pub fn parse_product_response(
    requested_product_id: u64,
    response: &[u8],
) -> Result<ProductDetail, RicohParseError> {
    if requested_product_id == 0 {
        return Err(RicohParseError::InvalidRequestedProductId);
    }
    let root = parse_envelope(response)?;
    let data = object(field(object(&root, "response")?, "data", "data")?, "data")?;
    let store_info = object(
        field(data, "storeInfo", "data.storeInfo")?,
        "data.storeInfo",
    )?;
    #[derive(serde::Deserialize)]
    struct DetailData<'a> {
        #[serde(rename = "storeInfo", borrow)]
        store_info: &'a RawValue,
    }
    let envelope: RawEnvelope<'_> =
        serde_json::from_slice(response).map_err(|_| RicohParseError::InvalidJson)?;
    let data: DetailData<'_> =
        serde_json::from_str(envelope.data.get()).map_err(|_| RicohParseError::InvalidJson)?;
    parse_product_item(
        store_info,
        data.store_info.get().as_bytes(),
        Some(requested_product_id),
    )
}

#[derive(serde::Deserialize)]
struct RawEnvelope<'a> {
    #[serde(borrow)]
    data: &'a RawValue,
}

pub fn parse_product_list_response(response: &[u8]) -> Result<Vec<ProductDetail>, RicohParseError> {
    let root = parse_envelope(response)?;
    let data = field(object(&root, "response")?, "data", "data")?;
    let items = data.as_array().ok_or(RicohParseError::InvalidType {
        field: "data",
        expected: "数组",
        actual: value_type(data),
    })?;
    if items.len() > MAX_PRODUCT_PAGE_ITEMS {
        return Err(RicohParseError::InvalidValue {
            field: "data",
            expected: "每页最多 20 件商品",
        });
    }
    let envelope: RawEnvelope<'_> =
        serde_json::from_slice(response).map_err(|_| RicohParseError::InvalidJson)?;
    let raw_items: Vec<&RawValue> =
        serde_json::from_str(envelope.data.get()).map_err(|_| RicohParseError::InvalidJson)?;
    let mut seen = BTreeSet::new();
    items
        .iter()
        .zip(raw_items)
        .map(|(item, raw)| {
            let detail = parse_product_item(object(item, "data[]")?, raw.get().as_bytes(), None)?;
            if detail.is_show != 1 {
                return Err(RicohParseError::InvalidValue {
                    field: "data[].isShow",
                    expected: "数值 1",
                });
            }
            if !seen.insert(detail.product_id) {
                return Err(RicohParseError::InvalidValue {
                    field: "data[].id",
                    expected: "每页商品编号不重复",
                });
            }
            Ok(detail)
        })
        .collect()
}

fn parse_envelope(response: &[u8]) -> Result<Value, RicohParseError> {
    if response.len() > MAX_RESPONSE_BYTES {
        return Err(RicohParseError::ResponseTooLarge {
            bytes: response.len(),
        });
    }

    let root: Value = serde_json::from_slice(response).map_err(|_| RicohParseError::InvalidJson)?;
    let code = field(object(&root, "response")?, "code", "code")?;
    let code = match code {
        Value::Number(number) => number.as_i64().ok_or(RicohParseError::InvalidValue {
            field: "code",
            expected: "整数业务码",
        })?,
        value => {
            return Err(RicohParseError::InvalidType {
                field: "code",
                expected: "整数业务码",
                actual: value_type(value),
            });
        }
    };
    if code != 0 {
        return Err(RicohParseError::BusinessCode { code });
    }

    Ok(root)
}

fn parse_product_item(
    store_info: &Map<String, Value>,
    raw_item: &[u8],
    requested_product_id: Option<u64>,
) -> Result<ProductDetail, RicohParseError> {
    let (name_path, id_path, show_path, stock_path) = if requested_product_id.is_some() {
        (
            "data.storeInfo.storeName",
            "data.storeInfo.id",
            "data.storeInfo.isShow",
            "data.storeInfo.stock",
        )
    } else {
        (
            "data[].storeName",
            "data[].id",
            "data[].isShow",
            "data[].stock",
        )
    };
    let name = match field(store_info, "storeName", name_path)? {
        Value::String(name) => name.clone(),
        value => {
            return Err(RicohParseError::InvalidType {
                field: name_path,
                expected: "字符串",
                actual: value_type(value),
            });
        }
    };
    if name.len() > MAX_PRODUCT_NAME_BYTES {
        return Err(RicohParseError::InvalidValue {
            field: name_path,
            expected: "长度不超过 1024 字节的商品名称",
        });
    }

    let response_product_id = match field(store_info, "id", id_path)? {
        Value::Number(number) => number
            .as_u64()
            .filter(|id| *id > 0 && *id <= i64::MAX as u64)
            .ok_or(RicohParseError::InvalidValue {
                field: id_path,
                expected: "正整数",
            })?,
        value => {
            return Err(RicohParseError::InvalidType {
                field: id_path,
                expected: "正整数",
                actual: value_type(value),
            });
        }
    };
    if let Some(requested) = requested_product_id {
        if response_product_id != requested {
            return Err(RicohParseError::ProductIdMismatch {
                requested,
                response: response_product_id,
            });
        }
    }

    let is_show = field(store_info, "isShow", show_path)?;
    let stock = field(store_info, "stock", stock_path)?;
    let stock_number = match stock {
        Value::Number(number) => number.clone(),
        value => {
            return Err(RicohParseError::InvalidType {
                field: stock_path,
                expected: "数值",
                actual: value_type(value),
            });
        }
    };
    if !stock_number
        .as_f64()
        .is_some_and(|stock| stock.is_finite() && stock >= 0.0)
        || is_negative_nonzero(&stock_number.to_string())
    {
        return Err(RicohParseError::InvalidValue {
            field: "stock",
            expected: "有限非负库存数值",
        });
    }
    let availability =
        evaluate_availability(Some(is_show), Some(stock)).map_err(map_availability_error)?;
    let is_show = match is_show {
        Value::Number(number) if number.as_f64() == Some(0.0) => 0,
        Value::Number(number) if number.as_f64() == Some(1.0) => 1,
        Value::Number(_) => {
            return Err(RicohParseError::InvalidValue {
                field: show_path,
                expected: "数值 0 或 1",
            });
        }
        value => {
            return Err(RicohParseError::InvalidType {
                field: show_path,
                expected: "数值 0 或 1",
                actual: value_type(value),
            });
        }
    };
    Ok(ProductDetail {
        product_id: response_product_id,
        name,
        is_show,
        stock: stock_number,
        availability,
        metadata: parse_metadata(store_info, raw_item),
    })
}

fn optional_text(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?.trim();
    (!text.is_empty() && text.len() <= MAX_METADATA_TEXT_BYTES).then(|| text.to_owned())
}

fn image_url(text: &str) -> Option<String> {
    let text = text.trim();
    if text.len() > MAX_IMAGE_URL_BYTES {
        return None;
    }
    let parsed = reqwest::Url::parse(text).ok()?;
    (matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some())
        .then(|| text.to_owned())
}

fn parse_metadata(store_info: &Map<String, Value>, response: &[u8]) -> ProductMetadata {
    let mut gallery_urls = Vec::new();
    if let Some(slider) = store_info.get("sliderImage").and_then(Value::as_str) {
        for url in slider.split(',').filter_map(image_url) {
            if !gallery_urls.contains(&url) {
                gallery_urls.push(url);
                if gallery_urls.len() == MAX_GALLERY_IMAGES {
                    break;
                }
            }
        }
    }
    ProductMetadata {
        image_url: store_info
            .get("image")
            .and_then(Value::as_str)
            .and_then(image_url),
        gallery_urls,
        price: original_price(store_info, response),
        unit_name: optional_text(store_info.get("unitName")),
        product_no: optional_text(store_info.get("productNo")),
        is_member_card: store_info.get("isMemberCard").and_then(Value::as_u64) == Some(1),
        member_card_days: store_info
            .get("memberCardDays")
            .and_then(Value::as_u64)
            .and_then(|days| u32::try_from(days).ok()),
    }
}

fn original_price(store_info: &Map<String, Value>, response: &[u8]) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Price<'a> {
        #[serde(borrow)]
        price: Option<&'a RawValue>,
    }

    let value = store_info.get("price")?.as_number()?.as_f64()?;
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    // 金额保留接口原始十进制文本，不经过二进制浮点转换。
    let item: Price<'_> = serde_json::from_slice(response).ok()?;
    let text = item.price?.get();
    (!is_negative_nonzero(text) && text.len() <= MAX_PRICE_TEXT_BYTES).then(|| text.to_owned())
}

fn is_negative_nonzero(text: &str) -> bool {
    text.starts_with('-')
        && text
            .split(['e', 'E'])
            .next()
            .unwrap_or_default()
            .bytes()
            .any(|digit| matches!(digit, b'1'..=b'9'))
}

fn object<'a>(
    value: &'a Value,
    field: &'static str,
) -> Result<&'a Map<String, Value>, RicohParseError> {
    match value {
        Value::Object(object) => Ok(object),
        value => Err(RicohParseError::InvalidType {
            field,
            expected: "对象",
            actual: value_type(value),
        }),
    }
}

fn field<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    path: &'static str,
) -> Result<&'a Value, RicohParseError> {
    object.get(key).ok_or(RicohParseError::MissingField(path))
}

fn value_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "空值",
        Value::Bool(_) => "布尔值",
        Value::Number(_) => "数值",
        Value::String(_) => "字符串",
        Value::Array(_) => "数组",
        Value::Object(_) => "对象",
    }
}

fn map_availability_error(error: AvailabilityError) -> RicohParseError {
    match error {
        AvailabilityError::MissingField(field) => RicohParseError::MissingField(field),
        AvailabilityError::InvalidType {
            field,
            expected,
            actual,
        } => RicohParseError::InvalidType {
            field,
            expected,
            actual,
        },
        AvailabilityError::InvalidValue { field, expected } => {
            RicohParseError::InvalidValue { field, expected }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn response(product_id: u64, is_show: Value, stock: Value) -> Vec<u8> {
        json!({
            "code": 0,
            "data": {
                "storeInfo": {
                    "storeName": "脱敏商品",
                    "id": product_id,
                    "isShow": is_show,
                    "stock": stock
                }
            }
        })
        .to_string()
        .into_bytes()
    }

    fn listed_response(items: Value) -> Vec<u8> {
        serde_json::to_vec(&json!({"code":0,"data":items})).unwrap()
    }

    #[test]
    fn list_parses_flat_fields_and_keeps_decimal_price_text() {
        let bytes = br#"{"code":0,"data":[{"id":108,"storeName":"VIP","isShow":1,"stock":0,"price":199.00,"image":"https://example.com/main.jpg","sliderImage":"https://example.com/main.jpg,https://example.com/main.jpg","isMemberCard":1,"memberCardDays":365}]}"#;
        let products = parse_product_list_response(bytes).unwrap();
        assert_eq!(products.len(), 1);
        assert_eq!(products[0].product_id, 108);
        assert_eq!(products[0].availability, Availability::OutOfStock);
        assert_eq!(products[0].metadata.price.as_deref(), Some("199.00"));
        assert_eq!(
            products[0].metadata.image_url.as_deref(),
            Some("https://example.com/main.jpg")
        );
        assert_eq!(products[0].metadata.gallery_urls.len(), 1);
        assert_eq!(products[0].metadata.member_card_days, Some(365));
    }

    #[test]
    fn list_empty_is_valid_but_invalid_envelopes_fail() {
        assert!(parse_product_list_response(br#"{"code":0,"data":[]}"#)
            .unwrap()
            .is_empty());
        for bytes in [
            br#"{"code":0,"data":null}"#.as_slice(),
            br#"{"code":0,"data":{}}"#,
            br#"{"code":0}"#,
            br#"{"code":"0","data":[]}"#,
            b"{",
        ] {
            assert!(parse_product_list_response(bytes).is_err());
        }
        assert_eq!(
            parse_product_list_response(br#"{"code":7001,"data":null}"#),
            Err(RicohParseError::BusinessCode { code: 7001 })
        );
    }

    #[test]
    fn list_rejects_invalid_items_and_duplicate_ids() {
        let item = json!({"id":41,"storeName":"商品","isShow":1,"stock":3});
        let mut largest = item.clone();
        largest["id"] = json!(i64::MAX);
        assert_eq!(
            parse_product_list_response(&listed_response(json!([largest]))).unwrap()[0].product_id,
            i64::MAX as u64
        );
        for (key, invalid) in [
            ("id", json!(0)),
            ("id", json!(u64::MAX)),
            ("id", json!("41")),
            ("isShow", json!(0)),
            ("isShow", json!(2)),
            ("stock", json!("3")),
            ("stock", json!(null)),
            ("stock", json!(-1)),
        ] {
            let mut invalid_item = item.clone();
            invalid_item[key] = invalid;
            assert!(
                parse_product_list_response(&listed_response(json!([invalid_item]))).is_err(),
                "{key}"
            );
        }
        assert!(
            parse_product_list_response(&listed_response(json!([item.clone(), item]))).is_err()
        );
        assert!(parse_product_list_response(
            br#"{"code":0,"data":[{"id":41,"storeName":"sample","isShow":1,"stock":1e1000}]}"#
        )
        .is_err());
        assert!(parse_product_list_response(
            br#"{"code":0,"data":[{"id":41,"storeName":"sample","isShow":1,"stock":-1e-1000}]}"#
        )
        .is_err());
        assert!(parse_product_list_response(&listed_response(json!([null]))).is_err());
    }

    #[test]
    fn list_accepts_invalid_optional_metadata_without_losing_valid_stock() {
        let parsed = parse_product_list_response(br#"{"code":0,"data":[{"id":41,"storeName":"sample","isShow":1,"stock":3,"price":1e1000,"image":23}]}"#).unwrap();
        assert_eq!(parsed[0].metadata.price, None);
        assert_eq!(parsed[0].metadata.image_url, None);
        assert_eq!(parsed[0].availability, Availability::InStock);
    }

    #[test]
    fn list_limits_item_count_and_decoded_response_size() {
        let items = (1..=20)
            .map(|id| json!({"id":id,"storeName":"商品","isShow":1,"stock":1}))
            .collect::<Vec<_>>();
        let mut bytes = listed_response(json!(items));
        bytes.resize(MAX_RESPONSE_BYTES, b' ');
        assert_eq!(parse_product_list_response(&bytes).unwrap().len(), 20);
        bytes.push(b' ');
        assert!(matches!(
            parse_product_list_response(&bytes),
            Err(RicohParseError::ResponseTooLarge { .. })
        ));
        let items = (1..=21)
            .map(|id| json!({"id":id,"storeName":"商品","isShow":1,"stock":1}))
            .collect::<Vec<_>>();
        assert!(parse_product_list_response(&listed_response(json!(items))).is_err());
    }

    #[test]
    fn parses_product_fixture_metadata_using_the_product_main_image() {
        for (id, bytes, price, unit, card, days) in [
            (
                65,
                include_bytes!("../tests/fixtures/product-65.json").as_slice(),
                "6749.0",
                "台",
                false,
                0,
            ),
            (
                130,
                include_bytes!("../tests/fixtures/product-130.json").as_slice(),
                "8819.0",
                "台",
                false,
                0,
            ),
            (
                108,
                include_bytes!("../tests/fixtures/product-108.json").as_slice(),
                "199.0",
                "张",
                true,
                365,
            ),
            (
                38,
                include_bytes!("../tests/fixtures/product-38.json").as_slice(),
                "6799.0",
                "台",
                false,
                0,
            ),
        ] {
            let detail = parse_product_response(id, bytes).unwrap();
            let body: Value = serde_json::from_slice(bytes).unwrap();
            assert_eq!(
                detail.metadata.image_url.as_deref(),
                body["data"]["storeInfo"]["image"].as_str()
            );
            assert_eq!(detail.metadata.price.as_deref(), Some(price));
            assert_eq!(detail.metadata.unit_name.as_deref(), Some(unit));
            assert_eq!(detail.metadata.is_member_card, card);
            assert_eq!(detail.metadata.member_card_days, Some(days));
            assert!(!detail.metadata.gallery_urls.is_empty());
            assert_ne!(
                detail.metadata.image_url.as_ref(),
                detail.metadata.gallery_urls.first()
            );
        }
    }

    #[test]
    fn price_keeps_decimal_text_without_binary_rounding() {
        for price in [
            "0",
            "0.00",
            "-0.00",
            "19.9900",
            "1234567890.123456789",
            "1.25e2",
        ] {
            let body = format!(
                r#"{{"code":0,"data":{{"storeInfo":{{"id":41,"storeName":"商品","isShow":1,"stock":3,"price":{price}}}}}}}"#
            );
            assert_eq!(
                parse_product_response(41, body.as_bytes())
                    .unwrap()
                    .metadata
                    .price
                    .as_deref(),
                Some(price)
            );
        }
    }

    #[test]
    fn negative_price_underflow_is_cleared_without_changing_stock() {
        let body = br#"{"code":0,"data":{"storeInfo":{"id":41,"storeName":"item","isShow":1,"stock":3,"price":-1e-1000}}}"#;
        let detail = parse_product_response(41, body).unwrap();
        assert_eq!(detail.metadata.price, None);
        assert_eq!(detail.availability, Availability::InStock);
    }

    #[test]
    fn invalid_optional_metadata_keeps_a_valid_stock_response() {
        let mut body: Value = serde_json::from_slice(&response(41, json!(1), json!(3))).unwrap();
        for (key, invalid) in [
            ("image", json!("data:image/png;base64,AAAA")),
            ("sliderImage", json!(["https://example.com/1.jpg"])),
            ("price", json!(-1)),
            ("unitName", json!("a".repeat(257))),
            ("productNo", json!(3)),
            ("isMemberCard", json!("1")),
            ("memberCardDays", json!(-5)),
        ] {
            body["data"]["storeInfo"][key] = invalid;
        }
        let detail = parse_product_response(41, &serde_json::to_vec(&body).unwrap()).unwrap();
        assert_eq!(detail.metadata, ProductMetadata::default());
        assert_eq!(detail.availability, Availability::InStock);
        for invalid in [json!("19.99"), json!(null), json!(-0.01)] {
            body["data"]["storeInfo"]["price"] = invalid;
            assert_eq!(
                parse_product_response(41, &serde_json::to_vec(&body).unwrap())
                    .unwrap()
                    .metadata
                    .price,
                None
            );
        }
        for price in ["1e1000".to_owned(), format!("0.{}1", "0".repeat(65))] {
            let bytes = format!(
                r#"{{"code":0,"data":{{"storeInfo":{{"id":41,"storeName":"商品","isShow":1,"stock":3,"price":{price}}}}}}}"#
            );
            let detail = parse_product_response(41, bytes.as_bytes()).unwrap();
            assert_eq!(detail.metadata.price, None);
            assert_eq!(detail.availability, Availability::InStock);
        }
    }

    #[test]
    fn gallery_deduplicates_and_stops_at_eight_valid_urls() {
        let mut body: Value = serde_json::from_slice(&response(41, json!(1), json!(3))).unwrap();
        body["data"]["storeInfo"]["sliderImage"] = json!(format!("invalid, https://example.com/1.jpg,https://example.com/1.jpg,{},https://example.com/2.jpg", (2..=12).map(|n| format!("https://example.com/{n}.jpg")).collect::<Vec<_>>().join(",")));
        body["data"]["storeInfo"]["image"] =
            json!(format!("https://example.com/{}", "a".repeat(2048)));
        let metadata = parse_product_response(41, &serde_json::to_vec(&body).unwrap())
            .unwrap()
            .metadata;
        assert_eq!(metadata.image_url, None);
        assert_eq!(
            metadata.gallery_urls,
            (1..=8)
                .map(|n| format!("https://example.com/{n}.jpg"))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn parses_unlisted_positive_stock_as_out_of_stock() {
        let parsed = parse_product_response(41, &response(41, json!(0), json!(3))).unwrap();
        assert_eq!(parsed.availability, Availability::OutOfStock);
        assert_eq!(parsed.stock, json!(3).as_number().unwrap().clone());
    }

    #[test]
    fn rejects_oversized_names_inside_an_otherwise_small_response() {
        let mut body: Value = serde_json::from_slice(&response(41, json!(1), json!(3))).unwrap();
        body["data"]["storeInfo"]["storeName"] = json!("a".repeat(MAX_PRODUCT_NAME_BYTES + 1));
        assert!(matches!(
            parse_product_response(41, &serde_json::to_vec(&body).unwrap()),
            Err(RicohParseError::InvalidValue {
                field: "data.storeInfo.storeName",
                ..
            })
        ));
        body["data"]["storeInfo"]["storeName"] = json!("a".repeat(MAX_PRODUCT_NAME_BYTES));
        assert!(parse_product_response(41, &serde_json::to_vec(&body).unwrap()).is_ok());
    }

    #[test]
    fn keeps_the_call_product_id_and_marks_listed_positive_stock_available() {
        let requested_id = 5_000_000_123;
        let parsed =
            parse_product_response(requested_id, &response(requested_id, json!(1), json!(2)))
                .unwrap();
        assert_eq!(parsed.product_id, requested_id);
        assert_eq!(parsed.name, "脱敏商品");
        assert_eq!(parsed.is_show, 1);
        assert_eq!(parsed.availability, Availability::InStock);
    }

    #[test]
    fn reports_unknown_business_codes_without_calling_them_missing_products() {
        let body = json!({"code": 7001, "data": null}).to_string();
        assert_eq!(
            parse_product_response(41, body.as_bytes()),
            Err(RicohParseError::BusinessCode { code: 7001 })
        );
    }

    #[test]
    fn rejects_invalid_json_missing_envelope_fields_and_non_integer_codes() {
        assert_eq!(
            parse_product_response(41, b"{"),
            Err(RicohParseError::InvalidJson)
        );
        assert_eq!(
            parse_product_response(41, b"{}"),
            Err(RicohParseError::MissingField("code"))
        );
        assert_eq!(
            parse_product_response(41, br#"{"code":0}"#),
            Err(RicohParseError::MissingField("data"))
        );
        assert_eq!(
            parse_product_response(41, br#"{"code":0,"data":{}}"#),
            Err(RicohParseError::MissingField("data.storeInfo"))
        );
        assert_eq!(
            parse_product_response(41, br#"{"code":"0","data":{}}"#),
            Err(RicohParseError::InvalidType {
                field: "code",
                expected: "整数业务码",
                actual: "字符串",
            })
        );
        assert_eq!(
            parse_product_response(41, br#"{"code":0.5,"data":{}}"#),
            Err(RicohParseError::InvalidValue {
                field: "code",
                expected: "整数业务码",
            })
        );
    }

    #[test]
    fn rejects_zero_requested_id_and_non_binary_listing_values() {
        assert_eq!(
            parse_product_response(0, &response(0, json!(1), json!(2))),
            Err(RicohParseError::InvalidRequestedProductId)
        );
        assert_eq!(
            parse_product_response(41, &response(41, json!(2), json!(3))),
            Err(RicohParseError::InvalidValue {
                field: "isShow",
                expected: "数值 0 或 1",
            })
        );
    }

    #[test]
    fn rejects_missing_and_invalid_store_fields() {
        let base = json!({
            "code": 0,
            "data": {"storeInfo": {
                "storeName": "脱敏商品", "id": 41, "isShow": 1, "stock": 3
            }}
        });
        for field in ["storeName", "id", "isShow", "stock"] {
            let mut missing = base.clone();
            missing["data"]["storeInfo"][field] = Value::Null;
            missing["data"]["storeInfo"]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(matches!(
                parse_product_response(41, missing.to_string().as_bytes()),
                Err(RicohParseError::MissingField(_))
            ));
        }

        for (field, invalid) in [
            ("storeName", json!(12)),
            ("id", json!("41")),
            ("isShow", json!("1")),
            ("stock", json!("3")),
        ] {
            let mut invalid_body = base.clone();
            invalid_body["data"]["storeInfo"][field] = invalid;
            assert!(matches!(
                parse_product_response(41, invalid_body.to_string().as_bytes()),
                Err(RicohParseError::InvalidType { .. })
            ));
        }
    }

    #[test]
    fn rejects_a_response_for_a_different_product_id() {
        assert_eq!(
            parse_product_response(41, &response(42, json!(1), json!(2))),
            Err(RicohParseError::ProductIdMismatch {
                requested: 41,
                response: 42,
            })
        );
    }

    #[test]
    fn enforces_the_decoded_response_size_boundary() {
        let mut exact = response(41, json!(1), json!(2));
        exact.resize(MAX_RESPONSE_BYTES, b' ');
        assert!(parse_product_response(41, &exact).is_ok());

        exact.push(b' ');
        assert_eq!(
            parse_product_response(41, &exact),
            Err(RicohParseError::ResponseTooLarge {
                bytes: MAX_RESPONSE_BYTES + 1,
            })
        );
    }
}
