DELETE FROM chart_of_accounts WHERE account_type IN (
    'user_custodial', 'user_payable', 'operational_fiat', 'operational_crypto',
    'treasury_fiat', 'treasury_crypto', 'fx_gain_loss', 'fee_revenue',
    'rail_settlement', 'venue_settlement', 'chargeback_reserve'
);