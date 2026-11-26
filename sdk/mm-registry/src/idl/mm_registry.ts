/**
 * Program IDL in camelCase format in order to be used in JS/TS.
 *
 * Note that this is only a type helper and is not the actual IDL. The original
 * IDL can be found at `target/idl/mm_registry.json`.
 */
export type MmRegistry = {
  "address": "MMReg11111111111111111111111111111111111111",
  "metadata": {
    "name": "mmRegistry",
    "version": "0.1.0",
    "spec": "0.1.0",
    "description": "Market Maker Registry for permissionless liquidity provision"
  },
  "instructions": [
    {
      "name": "cancelQuote",
      "docs": [
        "Cancel a quote"
      ],
      "discriminator": [
        41,
        51,
        237,
        236,
        153,
        159,
        210,
        185
      ],
      "accounts": [
        {
          "name": "owner",
          "writable": true,
          "signer": true
        },
        {
          "name": "registry",
          "writable": true
        },
        {
          "name": "marketMaker",
          "writable": true
        },
        {
          "name": "quote",
          "writable": true
        }
      ],
      "args": []
    },
    {
      "name": "depositCollateral",
      "docs": [
        "Deposit additional collateral"
      ],
      "discriminator": [
        156,
        131,
        142,
        116,
        146,
        247,
        162,
        120
      ],
      "accounts": [
        {
          "name": "owner",
          "writable": true,
          "signer": true
        },
        {
          "name": "registry"
        },
        {
          "name": "marketMaker",
          "writable": true
        },
        {
          "name": "mmCollateralAccount",
          "writable": true
        },
        {
          "name": "ownerTokenAccount",
          "writable": true
        },
        {
          "name": "tokenProgram"
        }
      ],
      "args": [
        {
          "name": "amount",
          "type": "u64"
        }
      ]
    },
    {
      "name": "deregisterMm",
      "docs": [
        "Deregister as a Market Maker"
      ],
      "discriminator": [
        151,
        175,
        138,
        184,
        217,
        152,
        207,
        250
      ],
      "accounts": [
        {
          "name": "owner",
          "writable": true,
          "signer": true
        },
        {
          "name": "registry",
          "writable": true
        },
        {
          "name": "marketMaker",
          "writable": true
        },
        {
          "name": "mmCollateralAccount",
          "writable": true
        },
        {
          "name": "ownerTokenAccount",
          "writable": true
        },
        {
          "name": "tokenProgram"
        }
      ],
      "args": []
    },
    {
      "name": "fillQuote",
      "docs": [
        "Fill a quote (called by order router or traders)"
      ],
      "discriminator": [
        12,
        116,
        225,
        132,
        142,
        74,
        167,
        253
      ],
      "accounts": [
        {
          "name": "taker",
          "docs": [
            "Also perps-core's payer: being in a Signer slot of the CPI is what makes",
            "the taker count towards the venue's `min_signers` (Anchor's CPI passes",
            "the UncheckedAccount `taker` slot as a non-signer)."
          ],
          "writable": true,
          "signer": true
        },
        {
          "name": "registry",
          "writable": true
        },
        {
          "name": "marketMaker",
          "writable": true
        },
        {
          "name": "quote",
          "writable": true
        },
        {
          "name": "mmOwner"
        },
        {
          "name": "venueAuthority"
        },
        {
          "name": "perpsConfig"
        },
        {
          "name": "perpsVenue",
          "writable": true
        },
        {
          "name": "venueProgramData"
        },
        {
          "name": "market",
          "docs": [
            "Never a typed Account here: the CPI mutates it."
          ],
          "writable": true
        },
        {
          "name": "pythPriceFeed"
        },
        {
          "name": "takerUser",
          "writable": true
        },
        {
          "name": "takerApproval",
          "writable": true
        },
        {
          "name": "takerPosition",
          "writable": true
        },
        {
          "name": "mmUser",
          "writable": true
        },
        {
          "name": "mmApproval",
          "writable": true
        },
        {
          "name": "mmPosition",
          "writable": true
        },
        {
          "name": "perpsLpPool",
          "writable": true
        },
        {
          "name": "perpsCoreProgram"
        },
        {
          "name": "systemProgram"
        }
      ],
      "args": [
        {
          "name": "params",
          "type": {
            "defined": {
              "name": "fillQuoteParams"
            }
          }
        }
      ],
      "returns": {
        "defined": {
          "name": "fillQuoteResult"
        }
      }
    },
    {
      "name": "initializeRegistry",
      "docs": [
        "Initialize the Market Maker Registry"
      ],
      "discriminator": [
        189,
        181,
        20,
        17,
        174,
        57,
        249,
        59
      ],
      "accounts": [
        {
          "name": "authority",
          "docs": [
            "Must be the perps-core admin: the registry PDA is one per market, so a",
            "permissionless init would let anyone claim it with their own mint/fees."
          ],
          "writable": true,
          "signer": true
        },
        {
          "name": "config"
        },
        {
          "name": "registry",
          "writable": true
        },
        {
          "name": "market",
          "docs": [
            "The perps-core market this registry serves (owner checked by Account)"
          ]
        },
        {
          "name": "collateralMint"
        },
        {
          "name": "systemProgram"
        },
        {
          "name": "tokenProgram"
        }
      ],
      "args": [
        {
          "name": "params",
          "type": {
            "defined": {
              "name": "initializeRegistryParams"
            }
          }
        }
      ]
    },
    {
      "name": "postQuote",
      "docs": [
        "Post a two-sided quote (bid and ask)"
      ],
      "discriminator": [
        68,
        231,
        88,
        224,
        13,
        116,
        27,
        84
      ],
      "accounts": [
        {
          "name": "owner",
          "writable": true,
          "signer": true
        },
        {
          "name": "registry",
          "writable": true
        },
        {
          "name": "marketMaker",
          "writable": true
        },
        {
          "name": "quote",
          "writable": true
        },
        {
          "name": "systemProgram"
        }
      ],
      "args": [
        {
          "name": "params",
          "type": {
            "defined": {
              "name": "postQuoteParams"
            }
          }
        }
      ]
    },
    {
      "name": "registerMm",
      "docs": [
        "Register as a Market Maker"
      ],
      "discriminator": [
        76,
        114,
        159,
        23,
        18,
        109,
        33,
        5
      ],
      "accounts": [
        {
          "name": "owner",
          "writable": true,
          "signer": true
        },
        {
          "name": "registry",
          "writable": true
        },
        {
          "name": "marketMaker",
          "writable": true
        },
        {
          "name": "mmCollateralAccount",
          "writable": true
        },
        {
          "name": "ownerTokenAccount",
          "writable": true
        },
        {
          "name": "collateralMint"
        },
        {
          "name": "systemProgram"
        },
        {
          "name": "tokenProgram"
        },
        {
          "name": "rent"
        }
      ],
      "args": [
        {
          "name": "initialCollateral",
          "type": "u64"
        }
      ]
    },
    {
      "name": "updateQuote",
      "docs": [
        "Update an existing quote"
      ],
      "discriminator": [
        235,
        69,
        162,
        233,
        147,
        53,
        42,
        225
      ],
      "accounts": [
        {
          "name": "owner",
          "writable": true,
          "signer": true
        },
        {
          "name": "registry"
        },
        {
          "name": "marketMaker",
          "writable": true
        },
        {
          "name": "quote",
          "writable": true
        }
      ],
      "args": [
        {
          "name": "params",
          "type": {
            "defined": {
              "name": "updateQuoteParams"
            }
          }
        }
      ]
    },
    {
      "name": "withdrawCollateral",
      "docs": [
        "Withdraw available collateral"
      ],
      "discriminator": [
        115,
        135,
        168,
        106,
        139,
        214,
        138,
        150
      ],
      "accounts": [
        {
          "name": "owner",
          "writable": true,
          "signer": true
        },
        {
          "name": "registry"
        },
        {
          "name": "marketMaker",
          "writable": true
        },
        {
          "name": "mmCollateralAccount",
          "writable": true
        },
        {
          "name": "ownerTokenAccount",
          "writable": true
        },
        {
          "name": "tokenProgram"
        }
      ],
      "args": [
        {
          "name": "amount",
          "type": "u64"
        }
      ]
    }
  ],
  "accounts": [
    {
      "name": "marketMaker",
      "discriminator": [
        151,
        123,
        40,
        58,
        68,
        211,
        104,
        56
      ]
    },
    {
      "name": "mmRegistry",
      "discriminator": [
        205,
        77,
        231,
        124,
        181,
        116,
        50,
        250
      ]
    },
    {
      "name": "twoSidedQuote",
      "discriminator": [
        106,
        93,
        221,
        190,
        77,
        6,
        44,
        17
      ]
    }
  ],
  "errors": [
    {
      "code": 6000,
      "name": "registryClosed",
      "msg": "Registry is not open for new registrations"
    },
    {
      "code": 6001,
      "name": "tradingDisabled",
      "msg": "Trading is not enabled"
    },
    {
      "code": 6002,
      "name": "insufficientCollateral",
      "msg": "Insufficient collateral"
    },
    {
      "code": 6003,
      "name": "marketMakerNotActive",
      "msg": "Market maker is not active"
    },
    {
      "code": 6004,
      "name": "marketMakerSuspended",
      "msg": "Market maker is suspended"
    },
    {
      "code": 6005,
      "name": "maxQuotesReached",
      "msg": "Maximum quotes reached"
    },
    {
      "code": 6006,
      "name": "quoteSizeTooSmall",
      "msg": "Quote size below minimum"
    },
    {
      "code": 6007,
      "name": "quoteSizeTooLarge",
      "msg": "Quote size above maximum"
    },
    {
      "code": 6008,
      "name": "spreadTooWide",
      "msg": "Quote spread exceeds maximum"
    },
    {
      "code": 6009,
      "name": "quoteExpired",
      "msg": "Quote has expired"
    },
    {
      "code": 6010,
      "name": "quoteNotActive",
      "msg": "Quote is not active"
    },
    {
      "code": 6011,
      "name": "fillSizeTooSmall",
      "msg": "Fill size too small"
    },
    {
      "code": 6012,
      "name": "fillSizeExceedsRemaining",
      "msg": "Fill size exceeds remaining"
    },
    {
      "code": 6013,
      "name": "invalidPrice",
      "msg": "Invalid price"
    },
    {
      "code": 6014,
      "name": "invalidQuoteParams",
      "msg": "Invalid quote parameters"
    },
    {
      "code": 6015,
      "name": "unauthorized",
      "msg": "Unauthorized"
    },
    {
      "code": 6016,
      "name": "mathOverflow",
      "msg": "Math overflow"
    },
    {
      "code": 6017,
      "name": "collateralLocked",
      "msg": "Cannot withdraw locked collateral"
    },
    {
      "code": 6018,
      "name": "hasActiveQuotes",
      "msg": "Has active quotes - cancel them first"
    },
    {
      "code": 6019,
      "name": "hasOpenInventory",
      "msg": "Has open inventory - close it first"
    },
    {
      "code": 6020,
      "name": "fillsDisabled",
      "msg": "Quote fills are disabled until they settle into perps-core positions"
    },
    {
      "code": 6021,
      "name": "invalidCollateralMint",
      "msg": "Collateral mint must be the market's collateral mint"
    },
    {
      "code": 6022,
      "name": "priceWorseThanLimit",
      "msg": "Quote price is worse than the taker's limit"
    },
    {
      "code": 6023,
      "name": "quoteNotFillable",
      "msg": "Legacy quote: set a leverage and expiry with update_quote"
    },
    {
      "code": 6024,
      "name": "selfTrade",
      "msg": "Taker cannot fill their own quote"
    },
    {
      "code": 6025,
      "name": "wrongMarket",
      "msg": "Market is not this registry's market"
    }
  ],
  "types": [
    {
      "name": "fillQuoteParams",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "size",
            "type": "u64"
          },
          {
            "name": "isBuy",
            "type": "bool"
          },
          {
            "name": "limitPrice",
            "type": "u64"
          },
          {
            "name": "takerLeverage",
            "type": "u32"
          },
          {
            "name": "allowPartial",
            "type": "bool"
          },
          {
            "name": "positionNonce",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "fillQuoteResult",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "fillPrice",
            "type": "u64"
          },
          {
            "name": "fillSize",
            "type": "u64"
          },
          {
            "name": "fee",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "initializeRegistryParams",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "minCollateral",
            "type": "u64"
          },
          {
            "name": "maxSpread",
            "type": "u32"
          },
          {
            "name": "minQuoteSize",
            "type": "u64"
          },
          {
            "name": "maxQuoteSize",
            "type": "u64"
          },
          {
            "name": "mmFee",
            "type": "u32"
          }
        ]
      }
    },
    {
      "name": "market",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "authority",
            "type": "pubkey"
          },
          {
            "name": "collateralMint",
            "type": "pubkey"
          },
          {
            "name": "vault",
            "type": "pubkey"
          },
          {
            "name": "pythPriceFeed",
            "type": "pubkey"
          },
          {
            "name": "commodity",
            "type": {
              "array": [
                "u8",
                8
              ]
            }
          },
          {
            "name": "maxLeverage",
            "type": "u32"
          },
          {
            "name": "maintenanceMarginRatio",
            "type": "u32"
          },
          {
            "name": "initialMarginRatio",
            "type": "u32"
          },
          {
            "name": "takerFee",
            "type": "u32"
          },
          {
            "name": "makerFee",
            "type": "u32"
          },
          {
            "name": "liquidationFee",
            "type": "u32"
          },
          {
            "name": "longOpenInterest",
            "type": "u64"
          },
          {
            "name": "shortOpenInterest",
            "type": "u64"
          },
          {
            "name": "maxOpenInterest",
            "type": "u64"
          },
          {
            "name": "cumulativeFunding",
            "type": "i64"
          },
          {
            "name": "lastFundingTime",
            "type": "i64"
          },
          {
            "name": "fundingInterval",
            "type": "i64"
          },
          {
            "name": "insuranceFund",
            "type": "u64"
          },
          {
            "name": "totalPositions",
            "type": "u64"
          },
          {
            "name": "totalTrades",
            "type": "u64"
          },
          {
            "name": "bump",
            "type": "u8"
          },
          {
            "name": "isPaused",
            "type": "bool"
          },
          {
            "name": "sessionSchedule",
            "type": "u8"
          },
          {
            "name": "preCloseWindowSecs",
            "type": "u32"
          },
          {
            "name": "weekendInitialMarginRatio",
            "type": "u32"
          },
          {
            "name": "weekendMaintenanceMarginRatio",
            "type": "u32"
          },
          {
            "name": "maxConfidenceBps",
            "type": "u32"
          },
          {
            "name": "minOraclePrice",
            "type": "u64"
          },
          {
            "name": "settlementPrice",
            "type": "u64"
          },
          {
            "name": "fundingRate",
            "type": "i64"
          },
          {
            "name": "fundingCarrySecs",
            "type": "u32"
          },
          {
            "name": "settledAt",
            "type": "i64"
          },
          {
            "name": "pythFeedId",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "oracleProgram",
            "type": "pubkey"
          },
          {
            "name": "pendingPriceFeed",
            "type": "pubkey"
          },
          {
            "name": "pendingFeedId",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "pendingOracleAt",
            "type": "i64"
          }
        ]
      }
    },
    {
      "name": "marketMaker",
      "docs": [
        "Individual Market Maker account"
      ],
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "owner",
            "docs": [
              "Owner/operator of this MM account"
            ],
            "type": "pubkey"
          },
          {
            "name": "registry",
            "docs": [
              "Registry this MM belongs to"
            ],
            "type": "pubkey"
          },
          {
            "name": "collateralAccount",
            "docs": [
              "MM's collateral token account"
            ],
            "type": "pubkey"
          },
          {
            "name": "collateralDeposited",
            "docs": [
              "Total collateral deposited. The vault is a registration bond only:",
              "fills margin from the MM's own perps-core UserAccount."
            ],
            "type": "u64"
          },
          {
            "name": "collateralLocked",
            "docs": [
              "Collateral locked in legacy quotes; only ever released now"
            ],
            "type": "u64"
          },
          {
            "name": "collateralAvailable",
            "docs": [
              "Available collateral (deposited - locked)"
            ],
            "type": "u64"
          },
          {
            "name": "inventory",
            "docs": [
              "Deprecated, always 0: fills open perps-core positions owned by the MM's",
              "wallet, which hold the inventory and PnL. Kept for the account layout."
            ],
            "type": "i64"
          },
          {
            "name": "avgInventoryPrice",
            "docs": [
              "Deprecated, always 0"
            ],
            "type": "u64"
          },
          {
            "name": "unrealizedPnl",
            "docs": [
              "Deprecated, always 0"
            ],
            "type": "i64"
          },
          {
            "name": "realizedPnl",
            "docs": [
              "Deprecated, always 0"
            ],
            "type": "i64"
          },
          {
            "name": "totalVolume",
            "docs": [
              "Total volume traded"
            ],
            "type": "u64"
          },
          {
            "name": "totalFills",
            "docs": [
              "Total number of fills"
            ],
            "type": "u64"
          },
          {
            "name": "totalFeesPaid",
            "docs": [
              "Total fees paid. No fee is charged at fill (perps-core charges the",
              "taker fee on close), so this no longer grows."
            ],
            "type": "u64"
          },
          {
            "name": "registeredAt",
            "docs": [
              "Registration timestamp"
            ],
            "type": "i64"
          },
          {
            "name": "lastActiveAt",
            "docs": [
              "Last activity timestamp"
            ],
            "type": "i64"
          },
          {
            "name": "activeQuotes",
            "docs": [
              "Number of active quotes"
            ],
            "type": "u8"
          },
          {
            "name": "maxQuotes",
            "docs": [
              "Maximum allowed active quotes"
            ],
            "type": "u8"
          },
          {
            "name": "status",
            "docs": [
              "Current status"
            ],
            "type": {
              "defined": {
                "name": "mmStatus"
              }
            }
          },
          {
            "name": "bump",
            "type": "u8"
          },
          {
            "name": "quotesPosted",
            "docs": [
              "Quotes ever posted; seeds the next quote PDA (active_quotes can repeat",
              "after a cancel, which made post_quote collide with a live quote)."
            ],
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "mmRegistry",
      "docs": [
        "Global configuration for the Market Maker Registry"
      ],
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "authority",
            "docs": [
              "Authority that can update registry parameters"
            ],
            "type": "pubkey"
          },
          {
            "name": "market",
            "docs": [
              "Market this registry serves"
            ],
            "type": "pubkey"
          },
          {
            "name": "collateralMint",
            "docs": [
              "Collateral mint (USDC)"
            ],
            "type": "pubkey"
          },
          {
            "name": "minCollateral",
            "docs": [
              "Minimum collateral required to register as MM"
            ],
            "type": "u64"
          },
          {
            "name": "maxSpread",
            "docs": [
              "Maximum spread allowed (basis points)"
            ],
            "type": "u32"
          },
          {
            "name": "minQuoteSize",
            "docs": [
              "Minimum quote size"
            ],
            "type": "u64"
          },
          {
            "name": "maxQuoteSize",
            "docs": [
              "Maximum quote size"
            ],
            "type": "u64"
          },
          {
            "name": "mmFee",
            "docs": [
              "Reserved: no fee is charged at fill. perps-core charges its taker fee",
              "on each leg when it is closed."
            ],
            "type": "u32"
          },
          {
            "name": "totalMms",
            "docs": [
              "Total registered market makers"
            ],
            "type": "u32"
          },
          {
            "name": "activeQuotes",
            "docs": [
              "Total active quotes"
            ],
            "type": "u32"
          },
          {
            "name": "totalVolume",
            "docs": [
              "Total volume traded through MMs"
            ],
            "type": "u64"
          },
          {
            "name": "totalFees",
            "docs": [
              "Total fees collected; no longer grows (see `mm_fee`)"
            ],
            "type": "u64"
          },
          {
            "name": "isOpen",
            "docs": [
              "Whether registry is accepting new MMs"
            ],
            "type": "bool"
          },
          {
            "name": "isTradingEnabled",
            "docs": [
              "Whether trading is enabled"
            ],
            "type": "bool"
          },
          {
            "name": "bump",
            "type": "u8"
          }
        ]
      }
    },
    {
      "name": "mmStatus",
      "type": {
        "kind": "enum",
        "variants": [
          {
            "name": "inactive"
          },
          {
            "name": "active"
          },
          {
            "name": "suspended"
          },
          {
            "name": "deregistered"
          }
        ]
      }
    },
    {
      "name": "postQuoteParams",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "bidPrice",
            "type": "u64"
          },
          {
            "name": "bidSize",
            "type": "u64"
          },
          {
            "name": "askPrice",
            "type": "u64"
          },
          {
            "name": "askSize",
            "type": "u64"
          },
          {
            "name": "minFillSize",
            "type": "u64"
          },
          {
            "name": "expiresIn",
            "type": "i64"
          },
          {
            "name": "leverage",
            "type": "u32"
          }
        ]
      }
    },
    {
      "name": "protocolConfig",
      "docs": [
        "Protocol-wide roles, created once by the program's upgrade authority.",
        "",
        "- `admin` creates markets and changes their settings (including settlement).",
        "It starts as the upgrade authority and can be handed to another key or a",
        "multisig with propose_admin / accept_admin.",
        "- `guardian` can only pause markets, for fast response in emergencies.",
        "- `oracle_program` is the Pyth Solana Receiver program: a market's price feed",
        "(a `PriceUpdateV2` account) must be owned by it when the market is created",
        "or its feed rotated; each market keeps a copy (`Market.oracle_program`,",
        "docs/lp-pool.md D47), so changing it here does not move existing markets.",
        "- `venue_fills_paused` stops every venue fill in every market. The guardian",
        "can set it; only the admin can clear it."
      ],
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "admin",
            "type": "pubkey"
          },
          {
            "name": "pendingAdmin",
            "type": "pubkey"
          },
          {
            "name": "guardian",
            "type": "pubkey"
          },
          {
            "name": "oracleProgram",
            "type": "pubkey"
          },
          {
            "name": "bump",
            "type": "u8"
          },
          {
            "name": "venueFillsPaused",
            "type": "bool"
          }
        ]
      }
    },
    {
      "name": "twoSidedQuote",
      "docs": [
        "Two-sided quote (bid and ask together)"
      ],
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "marketMaker",
            "docs": [
              "Market maker who posted this quote"
            ],
            "type": "pubkey"
          },
          {
            "name": "registry",
            "docs": [
              "Registry this quote belongs to"
            ],
            "type": "pubkey"
          },
          {
            "name": "bidPrice",
            "docs": [
              "Bid price (6 decimals)"
            ],
            "type": "u64"
          },
          {
            "name": "bidSize",
            "docs": [
              "Bid size"
            ],
            "type": "u64"
          },
          {
            "name": "bidRemaining",
            "docs": [
              "Bid remaining size"
            ],
            "type": "u64"
          },
          {
            "name": "askPrice",
            "docs": [
              "Ask price (6 decimals)"
            ],
            "type": "u64"
          },
          {
            "name": "askSize",
            "docs": [
              "Ask size"
            ],
            "type": "u64"
          },
          {
            "name": "askRemaining",
            "docs": [
              "Ask remaining size"
            ],
            "type": "u64"
          },
          {
            "name": "minFillSize",
            "docs": [
              "Minimum fill size"
            ],
            "type": "u64"
          },
          {
            "name": "collateralLocked",
            "docs": [
              "Collateral locked. Only legacy quotes have any: fills margin from the",
              "MM's perps-core account, so new quotes lock nothing."
            ],
            "type": "u64"
          },
          {
            "name": "createdAt",
            "docs": [
              "Quote creation timestamp"
            ],
            "type": "i64"
          },
          {
            "name": "expiresAt",
            "docs": [
              "Quote expiry timestamp. 0 only on legacy quotes, which cannot be filled."
            ],
            "type": "i64"
          },
          {
            "name": "updatedAt",
            "docs": [
              "Last update timestamp"
            ],
            "type": "i64"
          },
          {
            "name": "isActive",
            "docs": [
              "Whether quote is active"
            ],
            "type": "bool"
          },
          {
            "name": "bump",
            "type": "u8"
          },
          {
            "name": "leverage",
            "docs": [
              "The MM's leverage for fills of this quote (3 decimals, 10x = 10_000).",
              "0 only on legacy quotes, which cannot be filled."
            ],
            "type": "u32"
          }
        ]
      }
    },
    {
      "name": "updateQuoteParams",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "bidPrice",
            "type": {
              "option": "u64"
            }
          },
          {
            "name": "bidSize",
            "type": {
              "option": "u64"
            }
          },
          {
            "name": "askPrice",
            "type": {
              "option": "u64"
            }
          },
          {
            "name": "askSize",
            "type": {
              "option": "u64"
            }
          },
          {
            "name": "leverage",
            "type": {
              "option": "u32"
            }
          },
          {
            "name": "expiresIn",
            "type": {
              "option": "i64"
            }
          }
        ]
      }
    }
  ]
};
