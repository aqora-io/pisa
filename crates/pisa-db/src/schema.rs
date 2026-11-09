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
        replaces -> Nullable<Text>,
        replaces_allow -> Nullable<Text>,
        replacement -> Nullable<Text>,
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
        holder -> Nullable<Text>,
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

diesel::table! {
    jobs (id) {
        id -> BigInt,
        queue -> Text,
        version -> Nullable<Text>,
        priority -> SmallInt,
        header_key -> Nullable<Text>,
        header -> Nullable<Text>,
        affinity -> Nullable<Text>,
        job -> Jsonb,
        enqueued_micros -> BigInt,
        max_wait_micros -> BigInt,
        max_attempts -> Integer,
        worker -> Nullable<Text>,
        claimed_micros -> Nullable<BigInt>,
        lease_micros -> Nullable<BigInt>,
        attempts -> Integer,
        result -> Nullable<Jsonb>,
        done_micros -> Nullable<BigInt>,
        token -> Nullable<Text>,
        kind -> Nullable<Text>,
    }
}

diesel::table! {
    workers (queue, name) {
        queue -> Text,
        name -> Text,
        version -> Text,
        slots -> Integer,
        warm -> Jsonb,
        draining -> Bool,
        seen_micros -> BigInt,
    }
}
