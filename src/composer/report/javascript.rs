use std::collections::HashMap;

use anyhow::{anyhow, bail, Context, Result};
use quick_js::JsValue;
use serde_json::Value;

use crate::{
    composer::report::utils::get_oj_status, entities::SubmissionReportConfig, worker::run_container,
};

pub async fn execute_javascript_reporter(
    config: String,
    source: String,
) -> Result<SubmissionReportConfig> {
    tokio::task::spawn_blocking(move || {
        fn run(config: String, source: String) -> Result<SubmissionReportConfig> {
            let context = init_context(config).context("Error initializing the context")?;
            let source = format!("( function(DATA){{{source}}} )( JSON.parse(DATA) )");
            match context.eval(&source).context("Error executing the script")? {
                JsValue::Object(report) => Ok({
                    serde_json::from_value(
                        QuickJsObject(report)
                            .try_into()
                            .context("Error converting the returned object")?,
                    )
                    .context("Error deserializing the returned the object")?
                }),
                _ => bail!("Unknown return value by the reporter script"),
            }
        }

        run(config, source)
    })
    .await?
}

fn init_context(config: String) -> Result<quick_js::Context> {
    let context = quick_js::Context::new()?;
    context.set_global("DATA", JsValue::String(config))?;
    context.add_callback("getOJStatus", get_oj_status_wrapper)?;
    Ok(context)
}

fn get_oj_status_wrapper(
    run_report: HashMap<String, JsValue>,
    compare_report: HashMap<String, JsValue>,
) -> Result<&'static str> {
    use run_container::ExecutionReport;

    let run_report: ExecutionReport =
        serde_json::from_value(QuickJsObject(run_report).try_into()?)?;
    let compare_report: ExecutionReport =
        serde_json::from_value(QuickJsObject(compare_report).try_into()?)?;

    Ok(get_oj_status(run_report, compare_report).into())
}

struct QuickJsObject(HashMap<String, JsValue>);

impl TryInto<Value> for QuickJsObject {
    type Error = anyhow::Error;

    fn try_into(self) -> Result<Value> {
        do_convert(JsValue::Object(self.0))
    }
}

fn do_convert(value: JsValue) -> Result<Value> {
    use serde_json::Number;

    Ok(match value {
        JsValue::Undefined => Value::Null,
        JsValue::Null => Value::Null,
        JsValue::Bool(value) => Value::Bool(value),
        JsValue::Int(value) => Value::Number(Number::from(value)),
        JsValue::Float(value) => {
            Value::Number(Number::from_f64(value).ok_or_else(|| anyhow!("Invalid float number"))?)
        }
        JsValue::String(value) => Value::String(value),
        JsValue::Array(values) => {
            Value::Array(values.into_iter().map(do_convert).collect::<Result<_>>()?)
        }
        JsValue::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| do_convert(value).map(|value| (key, value)))
                .collect::<Result<_>>()?,
        ),
        _ => bail!("Unknown value detected"),
    })
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn test_execute_javascript_reporter() {
        let config = r#"{
            "submitted_at": "2023-01-28T12:48:09.155Z",
            "id": "complex",
            "steps": {
                "prepare": {
                    "status": "success",
                    "run_at": "2023-01-28T12:48:09.160Z",
                    "time_elapsed_ms": 0,
                    "report": null
                }
            }
        }"#
        .to_string();
        let source = "return {report:{str:'foo',num:114,float_num:114.514,obj:{bool:true},arr:[1,\
                      1,4,5,1,4]}}"
            .to_string();

        let report = super::execute_javascript_reporter(config, source).await.unwrap();
        let json = serde_json::to_string(&report).unwrap();

        assert_eq!(json, r#"{"report":{"arr":[1,1,4,5,1,4],"float_num":114.514,"num":114,"obj":{"bool":true},"str":"foo"},"embeds":[],"uploads":[]}"#.to_string());
    }
}
