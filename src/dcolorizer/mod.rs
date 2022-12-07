use std::error::Error;

use gstreamer::{glib, prelude::StaticType};

mod imp;

glib::wrapper! {
    pub struct DColorizer(ObjectSubclass<imp::DColorizer>) @extends gstreamer_base::BaseTransform, gstreamer::Element, gstreamer::Object;
}

pub fn register(plugin: &gstreamer::Plugin) -> Result<(), impl Error> {
    gstreamer::Element::register(
        Some(plugin),
        "dcolorizer",
        gstreamer::Rank::None,
        DColorizer::static_type(),
    )
}
