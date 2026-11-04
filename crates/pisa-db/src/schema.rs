//! The tables of both backends' migrations (`migrations/`), which must agree
//! with these.

diesel::table! {
    modules (version, id) {
        version -> Text,
        id -> Text,
        meta -> Jsonb,
        decls -> Nullable<Jsonb>,
    }
}

diesel::table! {
    module_imports (version, module, parent) {
        version -> Text,
        module -> Text,
        parent -> Text,
    }
}

diesel::table! {
    entries (version, id) {
        version -> Text,
        id -> Text,
        module -> Text,
        labels -> Jsonb,
        idempotency_key -> Nullable<Text>,
        created_micros -> BigInt,
    }
}

diesel::table! {
    described (version, module, name) {
        version -> Text,
        module -> Text,
        name -> Text,
        record -> Jsonb,
    }
}

diesel::table! {
    changes (id) {
        id -> BigInt,
        version -> Text,
        logged_micros -> BigInt,
        entry -> Nullable<Text>,
        module -> Nullable<Text>,
    }
}

diesel::table! {
    leases (version, name) {
        version -> Text,
        name -> Text,
        owner -> Text,
        until_micros -> BigInt,
    }
}

diesel::table! {
    states (version, hash) {
        version -> Text,
        hash -> Text,
        meta -> Jsonb,
        created -> BigInt,
    }
}

diesel::table! {
    sessions (version, id) {
        version -> Text,
        id -> Text,
        meta -> Jsonb,
        created -> BigInt,
    }
}
