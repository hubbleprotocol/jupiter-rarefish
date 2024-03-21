# Jupiter Rarefish Integration

This module implements the `Amm` trait defined [here](https://github.com/jup-ag/rust-amm-implementation).

The test `test_jupiter_rarefish_integration_quote` will print out a quote for selling 1000 USDC against the Rarefish mainnet SOL/USDC market and another example selling 1 USDH on the USDH/HBB market.
```
SWAP_PROGRAM_OWNER_FEE_ADDRESS=fiSha8e7EDkbxrWwfnTXGu7YQh9n4C52AHnEBBNEEYE cargo test -- test_jupiter_rarefish_integration_quote --nocapture
```

The test `test_jupiter_rarefish_integration_sim` will simulate a swap transaction, and if you want to actually run it have a `keypair.json` file that has both
```
SWAP_PROGRAM_OWNER_FEE_ADDRESS=fiSha8e7EDkbxrWwfnTXGu7YQh9n4C52AHnEBBNEEYE cargo test -- test_jupiter_rarefish_integration_sim --nocapture
```
