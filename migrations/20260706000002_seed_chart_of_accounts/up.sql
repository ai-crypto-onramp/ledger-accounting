-- Seed the versioned chart of accounts with the account types from the README.
-- version starts at 1; allowed_directions is an array of ('debit', 'credit').
-- For 'either' normal balance, both directions are allowed.

INSERT INTO chart_of_accounts (account_type, version, normal_balance, allowed_directions, description) VALUES
    ('user_custodial',     1, 'credit', ARRAY['credit'],          'Funds held on behalf of users (liability to the platform). Per-user, per-asset.'),
    ('user_payable',       1, 'credit', ARRAY['credit'],          'Funds owed but not yet credited to the user''s custodial account.'),
    ('operational_fiat',   1, 'debit',  ARRAY['debit'],           'Operational fiat float (settlement accounts, rail holdings).'),
    ('operational_crypto', 1, 'debit',  ARRAY['debit'],           'Operational crypto float (hot-wallet funding buffers).'),
    ('treasury_fiat',      1, 'debit',  ARRAY['debit'],           'Treasury fiat reserves and bank accounts.'),
    ('treasury_crypto',    1, 'debit',  ARRAY['debit'],           'Treasury crypto reserves (cold/warm custody).'),
    ('fx_gain_loss',       1, 'either', ARRAY['debit','credit'],  'Realized FX gains and losses from currency conversion.'),
    ('fee_revenue',        1, 'credit', ARRAY['credit'],          'Fee revenue recognized per transaction.'),
    ('rail_settlement',    1, 'debit',  ARRAY['debit'],           'In-transit funds on a payment rail awaiting settlement.'),
    ('venue_settlement',   1, 'debit',  ARRAY['debit'],           'In-transit funds at an exchange/OTC venue awaiting settlement.'),
    ('chargeback_reserve', 1, 'credit', ARRAY['credit'],          'Reserve for anticipated chargebacks and disputes.')
ON CONFLICT (account_type) DO NOTHING;