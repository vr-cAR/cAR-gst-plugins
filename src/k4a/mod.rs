use std::error::Error;

use gstreamer::{glib, prelude::StaticType};

mod imp;
mod libk4a;

glib::wrapper! {
    pub struct K4a(ObjectSubclass<imp::K4a>) @extends gstreamer_base::PushSrc, gstreamer_base::BaseSrc, gstreamer::Element, gstreamer::Object;
}

pub fn register(plugin: &gstreamer::Plugin) -> Result<(), impl Error> {
    gstreamer::Element::register(
        Some(plugin),
        "k4asrc",
        gstreamer::Rank::None,
        K4a::static_type(),
    )
}
