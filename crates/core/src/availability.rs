use serde_json::Value;

pub mod reducer;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Availability {
    InStock,
    OutOfStock,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AvailabilityError {
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
}

pub fn evaluate_availability(
    is_show: Option<&Value>,
    stock: Option<&Value>,
) -> Result<Availability, AvailabilityError> {
    let is_listed = match is_show.ok_or(AvailabilityError::MissingField("isShow"))? {
        Value::Number(value) if value.as_f64() == Some(1.0) => true,
        Value::Number(value) if value.as_f64() == Some(0.0) => false,
        // false 仅供已经规范化的数据源使用；理光适配器传入原始数值 0/1。
        Value::Bool(false) => false,
        Value::Number(_) => {
            return Err(AvailabilityError::InvalidValue {
                field: "isShow",
                expected: "数值 0 或 1",
            });
        }
        value => {
            return Err(AvailabilityError::InvalidType {
                field: "isShow",
                expected: "数值 0 或 1",
                actual: value_type(value),
            });
        }
    };

    let stock = stock.ok_or(AvailabilityError::MissingField("stock"))?;
    let stock = match stock {
        Value::Number(value) => value.as_f64().ok_or(AvailabilityError::InvalidValue {
            field: "stock",
            expected: "可表示的数值",
        })?,
        value => {
            return Err(AvailabilityError::InvalidType {
                field: "stock",
                expected: "数值",
                actual: value_type(value),
            });
        }
    };

    Ok(if is_listed && stock > 0.0 {
        Availability::InStock
    } else {
        Availability::OutOfStock
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn listed_product_with_positive_stock_is_in_stock() {
        assert_eq!(
            evaluate_availability(Some(&json!(1)), Some(&json!(3))),
            Ok(Availability::InStock)
        );
    }

    #[test]
    fn normalized_unlisted_product_is_out_of_stock() {
        assert_eq!(
            evaluate_availability(Some(&json!(false)), Some(&json!(3))),
            Ok(Availability::OutOfStock)
        );
    }

    #[test]
    fn raw_unlisted_product_with_positive_stock_is_out_of_stock() {
        assert_eq!(
            evaluate_availability(Some(&json!(0)), Some(&json!(3))),
            Ok(Availability::OutOfStock)
        );
    }

    #[test]
    fn unsupported_is_show_number_is_an_invalid_value() {
        assert_eq!(
            evaluate_availability(Some(&json!(2)), Some(&json!(3))),
            Err(AvailabilityError::InvalidValue {
                field: "isShow",
                expected: "数值 0 或 1",
            })
        );
    }

    #[test]
    fn listed_product_with_zero_stock_is_out_of_stock() {
        assert_eq!(
            evaluate_availability(Some(&json!(1)), Some(&json!(0))),
            Ok(Availability::OutOfStock)
        );
    }

    #[test]
    fn missing_fields_are_reported_by_name() {
        assert_eq!(
            evaluate_availability(None, Some(&json!(1))),
            Err(AvailabilityError::MissingField("isShow"))
        );
        assert_eq!(
            evaluate_availability(Some(&json!(1)), None),
            Err(AvailabilityError::MissingField("stock"))
        );
    }

    #[test]
    fn wrong_types_are_reported_without_numeric_coercion() {
        assert_eq!(
            evaluate_availability(Some(&json!("1")), Some(&json!(3))),
            Err(AvailabilityError::InvalidType {
                field: "isShow",
                expected: "数值 0 或 1",
                actual: "字符串",
            })
        );
        assert_eq!(
            evaluate_availability(Some(&json!(1)), Some(&json!("3"))),
            Err(AvailabilityError::InvalidType {
                field: "stock",
                expected: "数值",
                actual: "字符串",
            })
        );
    }
}
