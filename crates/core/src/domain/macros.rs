/// Declares a fieldless enum together with `ALL`, the list of its variants in
/// declaration order. Generating both from one list means `ALL` cannot fall
/// behind when a variant is added.
macro_rules! closed_enum {
    (
        $(#[$meta:meta])*
        $visibility:vis enum $name:ident {
            $(
                $(#[$variant_meta:meta])*
                $variant:ident
            ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        $visibility enum $name {
            $(
                $(#[$variant_meta])*
                $variant
            ),+
        }

        impl $name {
            /// Every variant, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
        }
    };
}

pub(crate) use closed_enum;
