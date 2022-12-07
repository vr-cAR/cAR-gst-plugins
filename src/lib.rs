#[cfg(feature = "dcolorizer")]
mod dcolorizer;
mod frame;
#[cfg(feature = "k4a")]
mod k4a;
mod macros;
#[cfg(feature = "theta")]
mod theta;

use std::error::Error;

use gstreamer::{self, plugin_define};

plugin_define!(
    c_ar_gst_plugins,
    "Video sources for cAR cameras",
    plugin_init,
    "0.1.0",
    "BSD",
    "c-ar-gst-plugins",
    "vr-cAr",
    "https://github.com/vr-cAR/cAr-gst-plugins",
    "2022-12-01"
);

fn plugin_init(plugin: &gstreamer::Plugin) -> Result<(), Box<dyn Error>> {
    #[cfg(feature = "theta")]
    theta::register(plugin)?;
    #[cfg(feature = "k4a")]
    k4a::register(plugin)?;
    #[cfg(feature = "dcolorizer")]
    dcolorizer::register(plugin)?;
    Ok(())
}
