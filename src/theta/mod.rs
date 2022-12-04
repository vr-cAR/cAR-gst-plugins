mod imp;
use std::error::Error;

use gstreamer::{glib, prelude::StaticType};

glib::wrapper! {
    pub struct ThetaUvc(ObjectSubclass<imp::ThetaUvc>) @extends gstreamer_base::PushSrc, gstreamer_base::BaseSrc, gstreamer::Element, gstreamer::Object;
}

pub fn register(plugin: &gstreamer::Plugin) -> Result<(), impl Error> {
    gstreamer::Element::register(
        Some(plugin),
        "thetauvcsrc",
        gstreamer::Rank::None,
        ThetaUvc::static_type(),
    )
}
