-- Set the default transaction isolation level to SERIALIZABLE for the ledger
-- role/database. The service asserts this at startup and refuses to boot if
-- weaker.
ALTER DATABASE ledger SET default_transaction_isolation = 'serializable';