use once_cell::sync::{Lazy, OnceCell};
use opentelemetry::{
    global,
    metrics::{Histogram, Meter, Unit},
    sdk::{metrics::controllers::BasicController, Resource},
    KeyValue,
};

use crate::conf;

pub static METRICS_RESOURCE: Lazy<Resource> = Lazy::new(|| {
    let mut pairs = vec![
        KeyValue::new("service.name", env!("CARGO_PKG_NAME")),
        KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
        KeyValue::new("host.name", conf::HOSTNAME.clone()),
    ];

    if let Some(container_name) = conf::CONTAINER_NAME.as_ref() {
        pairs.push(KeyValue::new("container.name", container_name.clone()));
    }

    if let Some(container_image_name) = conf::CONTAINER_IMAGE_NAME.as_ref() {
        pairs.push(KeyValue::new("container.image.name", container_image_name.clone()));
    }

    if let Some(container_image_tag) = conf::CONTAINER_IMAGE_TAG.as_ref() {
        pairs.push(KeyValue::new("container.image.tag", container_image_tag.clone()));
    }

    Resource::new(pairs)
});

pub static METRICS_CONTROLLER: OnceCell<BasicController> = OnceCell::new();

pub static METER: Lazy<Meter> =
    Lazy::new(|| global::meter_with_version(env!("CARGO_PKG_NAME"), Some("0.1"), None));

pub static SUBMISSION_HANDLING_HISTOGRAM: Lazy<Histogram<f64>> = Lazy::new(|| {
    METER
        .f64_histogram("submission.duration")
        .with_description("Duration of submissions handling")
        .with_unit(Unit::new("s"))
        .init()
});
