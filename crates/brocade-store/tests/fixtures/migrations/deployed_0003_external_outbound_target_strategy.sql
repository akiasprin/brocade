ALTER TABLE external_outbounds
    ADD COLUMN target_strategy TEXT,
    ADD CONSTRAINT external_outbounds_target_strategy_check CHECK (
        target_strategy IN ('as_is', 'use_ip', 'use_ipv4', 'use_ipv6', 'use_ipv4v6', 'use_ipv6v4')
    );
