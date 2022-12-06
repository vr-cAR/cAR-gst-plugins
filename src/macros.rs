macro_rules! set_field {
    ($cat:ident, $self:ident, $name:ident, $field:expr, ($val:expr)) => {
        {
            gstreamer::debug!(
                $cat,
                imp: $self,
                "Changing {} from {} to {}",
                Into::<&str>::into($name),
                $field,
                $val,
            );
            $field = $val;
        }
    };
    ($cat:ident, $self:ident, $name:ident, $field:expr, $value:ident) => {
        {
            let Ok(new_value) = $value.get() else {
                panic!("Could not deserialize value {}", Into::<&str>::into($name));
            };
            set_field!($cat, $self, $name, $field, (new_value));
        }
    };
    ($cat:ident, $self:ident, $name:ident, enum $field:expr, ($val:expr)) => {
        {
            gstreamer::debug!(
                $cat,
                imp: $self,
                "Changing {} from {} to {}",
                Into::<&str>::into($name),
                glib::EnumValue::from_value(&$field.to_value()).unwrap().1.name(),
                glib::EnumValue::from_value(&$val.to_value()).unwrap().1.name(),
            );
            $field = $val;
        }
    };
    ($cat:ident, $self:ident, $name:ident, enum $field:expr, $value:ident) => {
        {
            let Ok(new_value) = $value.get() else {
                panic!("Could not deserialize value passed in for {}", Into::<&str>::into($name));
            };
            {
                gstreamer::debug!(
                    $cat,
                    imp: $self,
                    "Changing {} from {} to {}",
                    Into::<&str>::into($name),
                    glib::EnumValue::from_value(&$field.to_value()).unwrap().1.name(),
                    glib::EnumValue::from_value(&$value).unwrap().1.name(),
                );
                $field = new_value;
            }
        }
    }
}

pub(crate) use set_field;
