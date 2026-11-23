//! The tables of both backends' migrations (`migrations/`), which must agree
//! with these.

diesel::table! {
    modules (version, id) {
        version -> Text,
        id -> Text,
        meta -> Jsonb,
        decls -> Nullable<Jsonb>,
        build -> Text,
        files -> Jsonb,
        header -> Text,
        status -> Text,
        verified -> Bool,
        created -> BigInt,
    }
}

diesel::table! {
    module_imports (version, module, parent) {
        version -> Text,
        module -> Text,
        parent -> Text,
        position -> Integer,
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
        holes -> Nullable<Text>,
        source -> Nullable<Text>,
        runs -> Nullable<Text>,
    }
}

diesel::table! {
    run_names (version, scope, name) {
        version -> Text,
        scope -> Text,
        name -> Text,
        root -> Text,
    }
}

diesel::table! {
    links (version, entry) {
        version -> Text,
        entry -> Text,
        replaces -> Text,
        allow -> Nullable<Text>,
        replacement -> Text,
        run_root -> Text,
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
    description_flags (id) {
        id -> BigInt,
        name -> Text,
        description_hash -> Text,
        reason -> Text,
        created_micros -> BigInt,
    }
}

diesel::table! {
    amendments (version, entry) {
        version -> Text,
        entry -> Text,
        amends -> Text,
        reason -> Text,
        changes -> Text,
        added -> Text,
        holes_before -> Nullable<Text>,
        holes_after -> Nullable<Text>,
    }
}

diesel::table! {
    refutations (version, entry) {
        version -> Text,
        entry -> Text,
        spec -> Text,
        target -> Text,
        theorem -> Text,
        target_hash -> Nullable<Text>,
        labels -> Text,
    }
}

diesel::table! {
    acceptance_specs (version, spec, acceptance) {
        version -> Text,
        spec -> Text,
        acceptance -> Text,
        created_micros -> BigInt,
    }
}

diesel::table! {
    acceptances (version, id) {
        version -> Text,
        id -> Text,
        created_micros -> BigInt,
        result -> Jsonb,
        labels -> Text,
    }
}

diesel::table! {
    catalogs (version) {
        version -> Text,
        id -> Text,
        created_micros -> BigInt,
        sweep_refused -> Nullable<Text>,
        sweep_refused_micros -> Nullable<BigInt>,
        clashes_checked_micros -> Nullable<BigInt>,
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
    states (version, id) {
        version -> Text,
        id -> Text,
        session -> Text,
        sorry -> Nullable<Integer>,
        parent -> Nullable<Text>,
        tactic -> Nullable<Text>,
        created -> BigInt,
    }
}

diesel::table! {
    sessions (version, id) {
        version -> Text,
        id -> Text,
        meta -> Jsonb,
        created -> BigInt,
        labels -> Text,
    }
}

diesel::table! {
    documents (version, id) {
        version -> Text,
        id -> Text,
        code -> Text,
        revision -> BigInt,
        holder -> Nullable<Text>,
        updated -> BigInt,
        timeout -> BigInt,
        labels -> Text,
        runs -> Nullable<Text>,
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
        document -> Nullable<Text>,
        labels -> Text,
        progress -> Nullable<Jsonb>,
    }
}

diesel::table! {
    expired_jobs (token) {
        token -> Text,
        queue -> Text,
        kind -> Text,
        labels -> Text,
        outcome -> Text,
        http_status -> Nullable<Integer>,
        finished_micros -> BigInt,
        swept_micros -> BigInt,
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
        beam_slots -> Integer,
        documents_open -> Integer,
        unlimited -> Bool,
        revision -> Text,
        store_reachable -> Nullable<Bool>,
        store_checked_micros -> Nullable<BigInt>,
        live_since_micros -> BigInt,
        memory_caps_mb -> Nullable<BigInt>,
        host_memory_mb -> BigInt,
        boot_id -> Text,
        toolchain -> Text,
        packages -> Text,
    }
}

diesel::allow_tables_to_appear_in_same_query!(entries, links);
diesel::allow_tables_to_appear_in_same_query!(jobs, workers);
