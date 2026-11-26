/**
 * Program IDL in camelCase format in order to be used in JS/TS.
 *
 * Note that this is only a type helper and is not the actual IDL. The original
 * IDL can be found at `target/idl/perps_core.json`.
 */
export type PerpsCore = {
  "address": "Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS",
  "metadata": {
    "name": "perpsCore",
    "version": "0.1.0",
    "spec": "0.1.0",
    "description": "Core perpetual futures trading engine"
  },
  "instructions": [
    {
      "name": "acceptAdmin",
      "discriminator": [
        112,
        42,
        45,
        90,
        116,
        181,
        13,
        170
      ],
      "accounts": [
        {
          "name": "newAdmin",
          "signer": true
        },
        {
          "name": "config",
          "writable": true
        }
      ],
      "args": []
    },
    {
      "name": "addMargin",
      "discriminator": [
        211,
        238,
        238,
        90,
        223,
        228,
        228,
        76
      ],
      "accounts": [
        {
          "name": "owner",
          "writable": true,
          "signer": true
        },
        {
          "name": "userAccount",
          "writable": true
        },
        {
          "name": "market"
        },
        {
          "name": "position",
          "writable": true
        },
        {
          "name": "lpPool",
          "writable": true
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
      "name": "adl",
      "discriminator": [
        54,
        233,
        120,
        58,
        42,
        177,
        121,
        5
      ],
      "accounts": [
        {
          "name": "signer",
          "signer": true
        },
        {
          "name": "config"
        },
        {
          "name": "market",
          "writable": true
        },
        {
          "name": "lpPool",
          "writable": true
        },
        {
          "name": "position",
          "writable": true
        },
        {
          "name": "positionOwner"
        },
        {
          "name": "userAccount",
          "writable": true
        },
        {
          "name": "pythPriceFeed"
        }
      ],
      "args": [
        {
          "name": "mode",
          "type": "u8"
        },
        {
          "name": "size",
          "type": "u64"
        }
      ]
    },
    {
      "name": "applyOracleFeed",
      "discriminator": [
        65,
        38,
        149,
        69,
        104,
        141,
        7,
        67
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true
        },
        {
          "name": "config"
        },
        {
          "name": "newPriceFeed"
        }
      ],
      "args": []
    },
    {
      "name": "applyPoolParams",
      "discriminator": [
        197,
        30,
        246,
        222,
        220,
        111,
        43,
        25
      ],
      "accounts": [
        {
          "name": "market"
        },
        {
          "name": "lpPool",
          "writable": true
        }
      ],
      "args": []
    },
    {
      "name": "applyReferralCode",
      "discriminator": [
        233,
        190,
        34,
        70,
        3,
        162,
        21,
        62
      ],
      "accounts": [
        {
          "name": "user",
          "writable": true,
          "signer": true
        },
        {
          "name": "referralCode",
          "writable": true
        },
        {
          "name": "userReferral",
          "writable": true
        },
        {
          "name": "systemProgram"
        }
      ],
      "args": []
    },
    {
      "name": "approveVenue",
      "discriminator": [
        254,
        60,
        62,
        30,
        179,
        64,
        176,
        184
      ],
      "accounts": [
        {
          "name": "owner",
          "writable": true,
          "signer": true
        },
        {
          "name": "market"
        },
        {
          "name": "venue"
        },
        {
          "name": "approval",
          "writable": true
        },
        {
          "name": "systemProgram"
        },
        {
          "name": "instructions"
        }
      ],
      "args": [
        {
          "name": "role",
          "type": "u8"
        },
        {
          "name": "params",
          "type": {
            "defined": {
              "name": "venueApprovalParams"
            }
          }
        }
      ]
    },
    {
      "name": "cancelOracleFeed",
      "discriminator": [
        60,
        11,
        122,
        55,
        74,
        236,
        160,
        8
      ],
      "accounts": [
        {
          "name": "admin",
          "signer": true
        },
        {
          "name": "config"
        },
        {
          "name": "market",
          "writable": true
        }
      ],
      "args": []
    },
    {
      "name": "cancelPoolParams",
      "discriminator": [
        62,
        255,
        249,
        254,
        234,
        230,
        184,
        162
      ],
      "accounts": [
        {
          "name": "admin",
          "signer": true
        },
        {
          "name": "config"
        },
        {
          "name": "lpPool",
          "writable": true
        }
      ],
      "args": []
    },
    {
      "name": "closePosition",
      "discriminator": [
        123,
        134,
        81,
        0,
        49,
        68,
        98,
        98
      ],
      "accounts": [
        {
          "name": "owner",
          "writable": true,
          "signer": true
        },
        {
          "name": "userAccount",
          "writable": true
        },
        {
          "name": "market",
          "writable": true
        },
        {
          "name": "position",
          "writable": true
        },
        {
          "name": "vault",
          "writable": true
        },
        {
          "name": "vaultTokenAccount",
          "writable": true
        },
        {
          "name": "userTokenAccount",
          "writable": true
        },
        {
          "name": "pythPriceFeed"
        },
        {
          "name": "tokenProgram"
        },
        {
          "name": "userReferral",
          "writable": true,
          "optional": true
        },
        {
          "name": "referralCode",
          "writable": true,
          "optional": true
        },
        {
          "name": "referrerTokenAccount",
          "docs": [
            "The referrer's token account; the reward is paid here at close.",
            "Validated in the handler (owner = referral code owner, market's mint).",
            "`dup`: the trader's own token account here is refused by that check",
            "(InvalidReferralParams), not by Anchor 1.x's duplicate-mutable check",
            "(2040); token accounts are not written back by Anchor, so the alias is",
            "harmless until the handler refuses it."
          ],
          "writable": true,
          "optional": true
        },
        {
          "name": "lpPool",
          "writable": true
        }
      ],
      "args": [
        {
          "name": "acceptablePrice",
          "type": "u64"
        }
      ]
    },
    {
      "name": "closeSettledPosition",
      "discriminator": [
        210,
        22,
        146,
        144,
        167,
        24,
        5,
        68
      ],
      "accounts": [
        {
          "name": "signer",
          "signer": true
        },
        {
          "name": "market",
          "writable": true
        },
        {
          "name": "lpPool",
          "writable": true
        },
        {
          "name": "position",
          "writable": true
        },
        {
          "name": "positionOwner"
        },
        {
          "name": "userAccount",
          "writable": true
        }
      ],
      "args": []
    },
    {
      "name": "createReferralCode",
      "discriminator": [
        206,
        2,
        37,
        2,
        193,
        190,
        203,
        191
      ],
      "accounts": [
        {
          "name": "owner",
          "writable": true,
          "signer": true
        },
        {
          "name": "referralCode",
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
              "name": "createReferralCodeParams"
            }
          }
        }
      ]
    },
    {
      "name": "depositCollateral",
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
          "name": "userAccount",
          "writable": true
        },
        {
          "name": "market"
        },
        {
          "name": "vault",
          "writable": true
        },
        {
          "name": "vaultTokenAccount",
          "writable": true
        },
        {
          "name": "userTokenAccount",
          "writable": true
        },
        {
          "name": "tokenProgram"
        },
        {
          "name": "lpPool",
          "writable": true
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
      "name": "disableVenue",
      "discriminator": [
        228,
        150,
        61,
        116,
        123,
        69,
        29,
        54
      ],
      "accounts": [
        {
          "name": "signer",
          "signer": true
        },
        {
          "name": "config"
        },
        {
          "name": "venue",
          "writable": true
        }
      ],
      "args": []
    },
    {
      "name": "donateToPool",
      "discriminator": [
        219,
        179,
        202,
        183,
        26,
        49,
        206,
        250
      ],
      "accounts": [
        {
          "name": "donor",
          "signer": true
        },
        {
          "name": "market"
        },
        {
          "name": "lpPool",
          "writable": true
        },
        {
          "name": "vaultTokenAccount",
          "writable": true
        },
        {
          "name": "donorTokenAccount",
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
      "name": "initLpPosition",
      "discriminator": [
        197,
        87,
        37,
        22,
        227,
        10,
        81,
        221
      ],
      "accounts": [
        {
          "name": "owner",
          "writable": true,
          "signer": true
        },
        {
          "name": "lpPool"
        },
        {
          "name": "lpPosition",
          "writable": true
        },
        {
          "name": "systemProgram"
        }
      ],
      "args": [
        {
          "name": "class",
          "type": "u8"
        }
      ]
    },
    {
      "name": "initializeConfig",
      "discriminator": [
        208,
        127,
        21,
        1,
        194,
        190,
        196,
        70
      ],
      "accounts": [
        {
          "name": "upgradeAuthority",
          "writable": true,
          "signer": true
        },
        {
          "name": "config",
          "writable": true
        },
        {
          "name": "programData",
          "docs": [
            "This program's ProgramData account (owned by the upgradeable loader,",
            "at the address derived from the program id)."
          ]
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
              "name": "protocolRolesParams"
            }
          }
        }
      ]
    },
    {
      "name": "initializeMarket",
      "discriminator": [
        35,
        35,
        189,
        193,
        155,
        48,
        170,
        203
      ],
      "accounts": [
        {
          "name": "authority",
          "docs": [
            "Must be the protocol admin; nobody else can create markets."
          ],
          "writable": true,
          "signer": true
        },
        {
          "name": "config"
        },
        {
          "name": "market",
          "writable": true
        },
        {
          "name": "vault",
          "writable": true
        },
        {
          "name": "vaultTokenAccount",
          "writable": true
        },
        {
          "name": "collateralMint"
        },
        {
          "name": "pythPriceFeed",
          "docs": [
            "by the configured oracle program; checked again in the handler"
          ]
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
          "name": "params",
          "type": {
            "defined": {
              "name": "initializeMarketParams"
            }
          }
        }
      ]
    },
    {
      "name": "initializePool",
      "discriminator": [
        95,
        180,
        10,
        172,
        84,
        174,
        232,
        40
      ],
      "accounts": [
        {
          "name": "admin",
          "writable": true,
          "signer": true
        },
        {
          "name": "config"
        },
        {
          "name": "market"
        },
        {
          "name": "lpPool",
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
              "name": "poolParams"
            }
          }
        },
        {
          "name": "adlKeeper",
          "type": "pubkey"
        }
      ]
    },
    {
      "name": "initializeUser",
      "discriminator": [
        111,
        17,
        185,
        250,
        60,
        122,
        38,
        254
      ],
      "accounts": [
        {
          "name": "owner",
          "writable": true,
          "signer": true
        },
        {
          "name": "market",
          "docs": [
            "User accounts are per market: collateral deposited in one market's vault",
            "can only be used and withdrawn there."
          ]
        },
        {
          "name": "userAccount",
          "writable": true
        },
        {
          "name": "systemProgram"
        }
      ],
      "args": []
    },
    {
      "name": "liquidate",
      "discriminator": [
        223,
        179,
        226,
        125,
        48,
        46,
        39,
        74
      ],
      "accounts": [
        {
          "name": "liquidator",
          "writable": true,
          "signer": true
        },
        {
          "name": "positionOwner"
        },
        {
          "name": "userAccount",
          "writable": true
        },
        {
          "name": "market",
          "writable": true
        },
        {
          "name": "position",
          "writable": true
        },
        {
          "name": "vault",
          "writable": true
        },
        {
          "name": "vaultTokenAccount",
          "writable": true
        },
        {
          "name": "liquidatorTokenAccount",
          "writable": true
        },
        {
          "name": "pythPriceFeed"
        },
        {
          "name": "tokenProgram"
        },
        {
          "name": "lpPool",
          "writable": true
        }
      ],
      "args": []
    },
    {
      "name": "lpCancelWithdrawal",
      "discriminator": [
        144,
        171,
        21,
        85,
        224,
        155,
        109,
        68
      ],
      "accounts": [
        {
          "name": "owner",
          "signer": true
        },
        {
          "name": "lpPool",
          "writable": true
        },
        {
          "name": "lpPosition",
          "writable": true
        }
      ],
      "args": []
    },
    {
      "name": "lpDeposit",
      "discriminator": [
        27,
        77,
        210,
        69,
        12,
        43,
        148,
        16
      ],
      "accounts": [
        {
          "name": "owner",
          "signer": true
        },
        {
          "name": "market"
        },
        {
          "name": "lpPool",
          "writable": true
        },
        {
          "name": "lpPosition",
          "writable": true
        },
        {
          "name": "vaultTokenAccount",
          "writable": true
        },
        {
          "name": "ownerTokenAccount",
          "writable": true
        },
        {
          "name": "pythPriceFeed"
        },
        {
          "name": "tokenProgram"
        }
      ],
      "args": [
        {
          "name": "amount",
          "type": "u64"
        },
        {
          "name": "minSharesOut",
          "type": "u64"
        }
      ]
    },
    {
      "name": "lpRequestWithdrawal",
      "discriminator": [
        105,
        199,
        161,
        213,
        244,
        47,
        176,
        3
      ],
      "accounts": [
        {
          "name": "owner",
          "signer": true
        },
        {
          "name": "market"
        },
        {
          "name": "lpPool",
          "writable": true
        },
        {
          "name": "lpPosition",
          "writable": true
        },
        {
          "name": "pythPriceFeed"
        }
      ],
      "args": [
        {
          "name": "shares",
          "type": "u64"
        }
      ]
    },
    {
      "name": "lpWithdraw",
      "discriminator": [
        205,
        206,
        130,
        170,
        173,
        51,
        11,
        169
      ],
      "accounts": [
        {
          "name": "owner",
          "signer": true
        },
        {
          "name": "market"
        },
        {
          "name": "lpPool",
          "writable": true
        },
        {
          "name": "lpPosition",
          "writable": true
        },
        {
          "name": "vault"
        },
        {
          "name": "vaultTokenAccount",
          "writable": true
        },
        {
          "name": "ownerTokenAccount",
          "writable": true
        },
        {
          "name": "pythPriceFeed"
        },
        {
          "name": "tokenProgram"
        }
      ],
      "args": []
    },
    {
      "name": "openPosition",
      "discriminator": [
        135,
        128,
        47,
        77,
        15,
        152,
        240,
        49
      ],
      "accounts": [
        {
          "name": "owner",
          "writable": true,
          "signer": true
        },
        {
          "name": "userAccount",
          "writable": true
        },
        {
          "name": "market",
          "writable": true
        },
        {
          "name": "position",
          "writable": true
        },
        {
          "name": "pythPriceFeed"
        },
        {
          "name": "systemProgram"
        },
        {
          "name": "lpPool",
          "writable": true
        }
      ],
      "args": [
        {
          "name": "params",
          "type": {
            "defined": {
              "name": "openPositionParams"
            }
          }
        }
      ]
    },
    {
      "name": "pauseMarket",
      "discriminator": [
        216,
        238,
        4,
        164,
        65,
        11,
        162,
        91
      ],
      "accounts": [
        {
          "name": "signer",
          "signer": true
        },
        {
          "name": "config"
        },
        {
          "name": "market",
          "writable": true
        }
      ],
      "args": []
    },
    {
      "name": "proposeAdmin",
      "discriminator": [
        121,
        214,
        199,
        212,
        87,
        39,
        117,
        234
      ],
      "accounts": [
        {
          "name": "admin",
          "signer": true
        },
        {
          "name": "config",
          "writable": true
        }
      ],
      "args": [
        {
          "name": "newAdmin",
          "type": "pubkey"
        }
      ]
    },
    {
      "name": "proposeOracleFeed",
      "discriminator": [
        177,
        89,
        122,
        118,
        250,
        142,
        6,
        90
      ],
      "accounts": [
        {
          "name": "admin",
          "signer": true
        },
        {
          "name": "config"
        },
        {
          "name": "market",
          "writable": true
        },
        {
          "name": "lpPool"
        },
        {
          "name": "newPriceFeed",
          "docs": [
            "the configured oracle program; parsed in the handler"
          ]
        }
      ],
      "args": [
        {
          "name": "feedId",
          "type": {
            "array": [
              "u8",
              32
            ]
          }
        }
      ]
    },
    {
      "name": "proposePoolParams",
      "discriminator": [
        57,
        33,
        221,
        55,
        150,
        196,
        88,
        233
      ],
      "accounts": [
        {
          "name": "admin",
          "signer": true
        },
        {
          "name": "config"
        },
        {
          "name": "market"
        },
        {
          "name": "lpPool",
          "writable": true
        }
      ],
      "args": [
        {
          "name": "params",
          "type": {
            "defined": {
              "name": "poolParams"
            }
          }
        }
      ]
    },
    {
      "name": "registerVenue",
      "discriminator": [
        116,
        155,
        199,
        243,
        228,
        180,
        176,
        173
      ],
      "accounts": [
        {
          "name": "admin",
          "writable": true,
          "signer": true
        },
        {
          "name": "config"
        },
        {
          "name": "market"
        },
        {
          "name": "venueProgram"
        },
        {
          "name": "venueProgramData"
        },
        {
          "name": "venue",
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
              "name": "venueParams"
            }
          }
        },
        {
          "name": "expectedUpgradeAuthority",
          "type": "pubkey"
        }
      ]
    },
    {
      "name": "revokeVenueApproval",
      "discriminator": [
        89,
        173,
        233,
        179,
        202,
        143,
        114,
        127
      ],
      "accounts": [
        {
          "name": "owner",
          "writable": true,
          "signer": true
        },
        {
          "name": "approval",
          "writable": true
        }
      ],
      "args": []
    },
    {
      "name": "setAdlKeeper",
      "discriminator": [
        207,
        213,
        90,
        152,
        85,
        90,
        74,
        31
      ],
      "accounts": [
        {
          "name": "admin",
          "signer": true
        },
        {
          "name": "config"
        },
        {
          "name": "lpPool",
          "writable": true
        }
      ],
      "args": [
        {
          "name": "key",
          "type": "pubkey"
        }
      ]
    },
    {
      "name": "setMaxPoolCash",
      "discriminator": [
        139,
        201,
        56,
        216,
        112,
        206,
        4,
        159
      ],
      "accounts": [
        {
          "name": "admin",
          "signer": true
        },
        {
          "name": "config"
        },
        {
          "name": "lpPool",
          "writable": true
        }
      ],
      "args": [
        {
          "name": "cap",
          "type": "u64"
        }
      ]
    },
    {
      "name": "setVenueFillsPaused",
      "discriminator": [
        210,
        98,
        170,
        182,
        79,
        54,
        186,
        72
      ],
      "accounts": [
        {
          "name": "signer",
          "signer": true
        },
        {
          "name": "config",
          "writable": true
        }
      ],
      "args": [
        {
          "name": "paused",
          "type": "bool"
        }
      ]
    },
    {
      "name": "settleMarket",
      "discriminator": [
        193,
        153,
        95,
        216,
        166,
        6,
        144,
        217
      ],
      "accounts": [
        {
          "name": "authority",
          "signer": true
        },
        {
          "name": "config"
        },
        {
          "name": "market",
          "writable": true
        }
      ],
      "args": [
        {
          "name": "settlementPrice",
          "type": "u64"
        }
      ]
    },
    {
      "name": "settlePositionPnl",
      "discriminator": [
        184,
        70,
        176,
        16,
        71,
        62,
        207,
        117
      ],
      "accounts": [
        {
          "name": "signer",
          "signer": true
        },
        {
          "name": "market"
        },
        {
          "name": "lpPool",
          "writable": true
        },
        {
          "name": "position",
          "writable": true
        },
        {
          "name": "pythPriceFeed"
        }
      ],
      "args": []
    },
    {
      "name": "sweepInsuranceExcess",
      "discriminator": [
        85,
        48,
        119,
        165,
        211,
        253,
        213,
        235
      ],
      "accounts": [
        {
          "name": "signer",
          "signer": true
        },
        {
          "name": "market",
          "writable": true
        },
        {
          "name": "lpPool",
          "writable": true
        }
      ],
      "args": []
    },
    {
      "name": "updateFunding",
      "discriminator": [
        224,
        66,
        9,
        70,
        75,
        81,
        94,
        88
      ],
      "accounts": [
        {
          "name": "keeper",
          "writable": true,
          "signer": true
        },
        {
          "name": "market",
          "writable": true
        },
        {
          "name": "pythPriceFeed"
        }
      ],
      "args": []
    },
    {
      "name": "updateMarketConfig",
      "discriminator": [
        91,
        87,
        149,
        101,
        110,
        116,
        16,
        120
      ],
      "accounts": [
        {
          "name": "authority",
          "signer": true
        },
        {
          "name": "config"
        },
        {
          "name": "market",
          "writable": true
        }
      ],
      "args": [
        {
          "name": "params",
          "type": {
            "defined": {
              "name": "marketConfigParams"
            }
          }
        }
      ]
    },
    {
      "name": "updateRoles",
      "discriminator": [
        220,
        152,
        205,
        233,
        177,
        123,
        219,
        125
      ],
      "accounts": [
        {
          "name": "admin",
          "signer": true
        },
        {
          "name": "config",
          "writable": true
        }
      ],
      "args": [
        {
          "name": "params",
          "type": {
            "defined": {
              "name": "protocolRolesParams"
            }
          }
        }
      ]
    },
    {
      "name": "updateVenue",
      "discriminator": [
        148,
        1,
        158,
        184,
        188,
        145,
        146,
        39
      ],
      "accounts": [
        {
          "name": "admin",
          "signer": true
        },
        {
          "name": "config"
        },
        {
          "name": "venue",
          "writable": true
        },
        {
          "name": "venueProgramData"
        }
      ],
      "args": [
        {
          "name": "params",
          "type": {
            "defined": {
              "name": "venueParams"
            }
          }
        },
        {
          "name": "enabled",
          "type": "bool"
        },
        {
          "name": "expectedUpgradeAuthority",
          "type": "pubkey"
        }
      ]
    },
    {
      "name": "updateVenueApproval",
      "discriminator": [
        243,
        224,
        3,
        21,
        150,
        246,
        103,
        7
      ],
      "accounts": [
        {
          "name": "owner",
          "signer": true
        },
        {
          "name": "market"
        },
        {
          "name": "venue"
        },
        {
          "name": "approval",
          "writable": true
        },
        {
          "name": "instructions"
        }
      ],
      "args": [
        {
          "name": "params",
          "type": {
            "defined": {
              "name": "venueApprovalParams"
            }
          }
        }
      ]
    },
    {
      "name": "venueFill",
      "discriminator": [
        154,
        69,
        106,
        64,
        165,
        125,
        152,
        141
      ],
      "accounts": [
        {
          "name": "venueAuthority",
          "signer": true
        },
        {
          "name": "payer",
          "writable": true,
          "signer": true
        },
        {
          "name": "config"
        },
        {
          "name": "market",
          "writable": true
        },
        {
          "name": "venue",
          "writable": true
        },
        {
          "name": "venueProgramData"
        },
        {
          "name": "pythPriceFeed"
        },
        {
          "name": "taker"
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
          "name": "maker",
          "docs": [
            "and approvals aliasing."
          ]
        },
        {
          "name": "makerUser",
          "writable": true
        },
        {
          "name": "makerApproval",
          "writable": true
        },
        {
          "name": "makerPosition",
          "writable": true
        },
        {
          "name": "systemProgram"
        },
        {
          "name": "lpPool",
          "writable": true
        }
      ],
      "args": [
        {
          "name": "params",
          "type": {
            "defined": {
              "name": "venueFillParams"
            }
          }
        }
      ]
    },
    {
      "name": "withdrawCollateral",
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
          "name": "userAccount",
          "writable": true
        },
        {
          "name": "market"
        },
        {
          "name": "vault",
          "writable": true
        },
        {
          "name": "vaultTokenAccount",
          "writable": true
        },
        {
          "name": "userTokenAccount",
          "writable": true
        },
        {
          "name": "tokenProgram"
        },
        {
          "name": "lpPool",
          "writable": true
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
      "name": "withdrawTreasury",
      "discriminator": [
        40,
        63,
        122,
        158,
        144,
        216,
        83,
        96
      ],
      "accounts": [
        {
          "name": "admin",
          "signer": true
        },
        {
          "name": "config"
        },
        {
          "name": "market"
        },
        {
          "name": "lpPool",
          "writable": true
        },
        {
          "name": "vault"
        },
        {
          "name": "vaultTokenAccount",
          "writable": true
        },
        {
          "name": "destination",
          "docs": [
            "Not the vault itself: a self-transfer moves no tokens, so the",
            "treasury would leave the books and strand its tokens as surplus.",
            "`dup` keeps that refusal VaultInvariantViolated rather than Anchor",
            "1.x's duplicate-mutable 2040 (the constraint below still refuses it)."
          ],
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
      "name": "writeOffBadDebt",
      "discriminator": [
        166,
        5,
        239,
        3,
        173,
        87,
        155,
        155
      ],
      "accounts": [
        {
          "name": "signer",
          "signer": true
        },
        {
          "name": "market"
        },
        {
          "name": "lpPool",
          "writable": true
        }
      ],
      "args": []
    }
  ],
  "accounts": [
    {
      "name": "lpPool",
      "discriminator": [
        185,
        127,
        131,
        141,
        197,
        198,
        170,
        147
      ]
    },
    {
      "name": "lpPosition",
      "discriminator": [
        105,
        241,
        37,
        200,
        224,
        2,
        252,
        90
      ]
    },
    {
      "name": "market",
      "discriminator": [
        219,
        190,
        213,
        55,
        0,
        227,
        198,
        154
      ]
    },
    {
      "name": "position",
      "discriminator": [
        170,
        188,
        143,
        228,
        122,
        64,
        247,
        208
      ]
    },
    {
      "name": "protocolConfig",
      "discriminator": [
        207,
        91,
        250,
        28,
        152,
        179,
        215,
        209
      ]
    },
    {
      "name": "referralCode",
      "discriminator": [
        227,
        239,
        247,
        224,
        128,
        187,
        44,
        229
      ]
    },
    {
      "name": "userAccount",
      "discriminator": [
        211,
        33,
        136,
        16,
        186,
        110,
        242,
        127
      ]
    },
    {
      "name": "userReferral",
      "discriminator": [
        60,
        72,
        219,
        164,
        122,
        102,
        170,
        140
      ]
    },
    {
      "name": "vault",
      "discriminator": [
        211,
        8,
        232,
        43,
        2,
        152,
        117,
        119
      ]
    },
    {
      "name": "venue",
      "discriminator": [
        8,
        155,
        85,
        226,
        234,
        173,
        42,
        242
      ]
    },
    {
      "name": "venueApproval",
      "discriminator": [
        159,
        236,
        74,
        239,
        92,
        187,
        160,
        63
      ]
    }
  ],
  "events": [
    {
      "name": "adl",
      "discriminator": [
        41,
        101,
        190,
        34,
        254,
        161,
        140,
        225
      ]
    },
    {
      "name": "badDebt",
      "discriminator": [
        192,
        33,
        54,
        41,
        223,
        158,
        37,
        34
      ]
    },
    {
      "name": "badDebtWrittenOff",
      "discriminator": [
        236,
        46,
        22,
        26,
        81,
        51,
        171,
        41
      ]
    },
    {
      "name": "fundingUpdated",
      "discriminator": [
        206,
        76,
        89,
        81,
        126,
        37,
        255,
        224
      ]
    },
    {
      "name": "insuranceSwept",
      "discriminator": [
        71,
        241,
        247,
        53,
        59,
        217,
        227,
        251
      ]
    },
    {
      "name": "lpDeposited",
      "discriminator": [
        85,
        211,
        184,
        159,
        176,
        224,
        28,
        72
      ]
    },
    {
      "name": "lpWithdrawalCancelled",
      "discriminator": [
        238,
        119,
        21,
        97,
        126,
        192,
        147,
        158
      ]
    },
    {
      "name": "lpWithdrawalRequested",
      "discriminator": [
        246,
        15,
        59,
        159,
        176,
        116,
        178,
        199
      ]
    },
    {
      "name": "lpWithdrawn",
      "discriminator": [
        188,
        10,
        43,
        60,
        223,
        238,
        51,
        153
      ]
    },
    {
      "name": "oracleFeedApplied",
      "discriminator": [
        72,
        171,
        129,
        2,
        135,
        88,
        200,
        157
      ]
    },
    {
      "name": "oracleFeedCancelled",
      "discriminator": [
        154,
        13,
        86,
        41,
        184,
        70,
        130,
        239
      ]
    },
    {
      "name": "oracleFeedProposed",
      "discriminator": [
        5,
        121,
        161,
        219,
        121,
        33,
        131,
        227
      ]
    },
    {
      "name": "poolDonated",
      "discriminator": [
        44,
        83,
        248,
        99,
        189,
        118,
        71,
        59
      ]
    },
    {
      "name": "poolParamsApplied",
      "discriminator": [
        89,
        224,
        188,
        156,
        114,
        55,
        8,
        35
      ]
    },
    {
      "name": "poolParamsCancelled",
      "discriminator": [
        142,
        29,
        65,
        178,
        85,
        41,
        216,
        19
      ]
    },
    {
      "name": "poolParamsProposed",
      "discriminator": [
        145,
        12,
        67,
        250,
        71,
        29,
        93,
        17
      ]
    },
    {
      "name": "positionClosed",
      "discriminator": [
        157,
        163,
        227,
        228,
        13,
        97,
        138,
        121
      ]
    },
    {
      "name": "positionLiquidated",
      "discriminator": [
        40,
        107,
        90,
        214,
        96,
        30,
        61,
        128
      ]
    },
    {
      "name": "positionOpened",
      "discriminator": [
        237,
        175,
        243,
        230,
        147,
        117,
        101,
        121
      ]
    },
    {
      "name": "positionPnlSettled",
      "discriminator": [
        38,
        91,
        40,
        29,
        124,
        10,
        150,
        53
      ]
    },
    {
      "name": "treasuryWithdrawn",
      "discriminator": [
        143,
        181,
        157,
        169,
        87,
        155,
        170,
        46
      ]
    },
    {
      "name": "venueFilled",
      "discriminator": [
        219,
        143,
        140,
        156,
        178,
        113,
        95,
        144
      ]
    }
  ],
  "errors": [
    {
      "code": 6000,
      "name": "marketPaused",
      "msg": "Market is currently paused"
    },
    {
      "code": 6001,
      "name": "excessiveLeverage",
      "msg": "Leverage exceeds maximum allowed"
    },
    {
      "code": 6002,
      "name": "insufficientCollateral",
      "msg": "Insufficient collateral for position"
    },
    {
      "code": 6003,
      "name": "positionTooSmall",
      "msg": "Position size is too small"
    },
    {
      "code": 6004,
      "name": "positionTooLarge",
      "msg": "Position size exceeds maximum"
    },
    {
      "code": 6005,
      "name": "openInterestCapExceeded",
      "msg": "Open interest cap would be exceeded"
    },
    {
      "code": 6006,
      "name": "belowMaintenanceMargin",
      "msg": "Position is below maintenance margin"
    },
    {
      "code": 6007,
      "name": "notLiquidatable",
      "msg": "Position is not liquidatable"
    },
    {
      "code": 6008,
      "name": "invalidOraclePrice",
      "msg": "Invalid oracle price"
    },
    {
      "code": 6009,
      "name": "staleOraclePrice",
      "msg": "Oracle price is stale"
    },
    {
      "code": 6010,
      "name": "invalidPositionSide",
      "msg": "Invalid position side"
    },
    {
      "code": 6011,
      "name": "positionNotFound",
      "msg": "Position not found"
    },
    {
      "code": 6012,
      "name": "positionAlreadyClosed",
      "msg": "Position is already closed"
    },
    {
      "code": 6013,
      "name": "unauthorized",
      "msg": "Unauthorized access"
    },
    {
      "code": 6014,
      "name": "mathOverflow",
      "msg": "Math overflow"
    },
    {
      "code": 6015,
      "name": "invalidMarketConfig",
      "msg": "Invalid market configuration"
    },
    {
      "code": 6016,
      "name": "invalidPositionReduction",
      "msg": "Cannot reduce position below zero"
    },
    {
      "code": 6017,
      "name": "insufficientVaultBalance",
      "msg": "Insufficient vault balance"
    },
    {
      "code": 6018,
      "name": "invalidReferralCode",
      "msg": "Invalid referral code format"
    },
    {
      "code": 6019,
      "name": "referralCodeExists",
      "msg": "Referral code already exists"
    },
    {
      "code": 6020,
      "name": "referralCodeNotFound",
      "msg": "Referral code not found"
    },
    {
      "code": 6021,
      "name": "selfReferralNotAllowed",
      "msg": "Cannot use your own referral code"
    },
    {
      "code": 6022,
      "name": "referralAlreadyApplied",
      "msg": "User already has a referral applied"
    },
    {
      "code": 6023,
      "name": "userAlreadyHasCode",
      "msg": "User already has a referral code"
    },
    {
      "code": 6024,
      "name": "noRewardsToClaim",
      "msg": "No rewards available to claim"
    },
    {
      "code": 6025,
      "name": "referralCodeInactive",
      "msg": "Referral code is inactive"
    },
    {
      "code": 6026,
      "name": "invalidReferralParams",
      "msg": "Invalid referral parameters"
    },
    {
      "code": 6027,
      "name": "marketClosed",
      "msg": "Market is outside trading hours"
    },
    {
      "code": 6028,
      "name": "marketSettled",
      "msg": "Market has been settled; positions can only be closed"
    },
    {
      "code": 6029,
      "name": "oracleConfidenceTooWide",
      "msg": "Oracle confidence interval is too wide"
    },
    {
      "code": 6030,
      "name": "oraclePriceBelowFloor",
      "msg": "Oracle price is below the market's minimum price"
    },
    {
      "code": 6031,
      "name": "invalidOracleAccount",
      "msg": "Price feed is not owned by the configured oracle program"
    },
    {
      "code": 6032,
      "name": "venueUnauthorized",
      "msg": "Signer is not the venue's authority"
    },
    {
      "code": 6033,
      "name": "venueDisabled",
      "msg": "Venue is disabled"
    },
    {
      "code": 6034,
      "name": "venueProgramChanged",
      "msg": "Venue program was upgraded since it was registered; the admin must re-attest"
    },
    {
      "code": 6035,
      "name": "venueFillsPaused",
      "msg": "Venue fills are paused protocol-wide"
    },
    {
      "code": 6036,
      "name": "invalidVenueParams",
      "msg": "Invalid venue parameters"
    },
    {
      "code": 6037,
      "name": "invalidFillParams",
      "msg": "Invalid fill parameters"
    },
    {
      "code": 6038,
      "name": "fillPriceOutOfBand",
      "msg": "Fill price is outside the venue's band around the oracle"
    },
    {
      "code": 6039,
      "name": "fillTooLarge",
      "msg": "Fill notional exceeds the venue's per-fill cap"
    },
    {
      "code": 6040,
      "name": "selfTrade",
      "msg": "Taker and maker are the same wallet"
    },
    {
      "code": 6041,
      "name": "missingSigner",
      "msg": "Not enough of taker and maker signed"
    },
    {
      "code": 6042,
      "name": "approvalExpired",
      "msg": "Venue approval has expired"
    },
    {
      "code": 6043,
      "name": "approvalAllowanceExceeded",
      "msg": "Fill would commit more collateral than the approval allows"
    },
    {
      "code": 6044,
      "name": "approvalLeverageExceeded",
      "msg": "Leverage exceeds the approval's maximum"
    },
    {
      "code": 6045,
      "name": "approvalLimitPrice",
      "msg": "Fill price is beyond the approval's limit price"
    },
    {
      "code": 6046,
      "name": "approvalPriceDeviation",
      "msg": "Fill price deviates too far from the oracle for the approval"
    },
    {
      "code": 6047,
      "name": "invalidApprovalParams",
      "msg": "Invalid venue approval parameters"
    },
    {
      "code": 6048,
      "name": "venueMarginInvariant",
      "msg": "Venue fill would open a position below initial margin"
    },
    {
      "code": 6049,
      "name": "approvalSideNotAllowed",
      "msg": "The approval does not allow this side"
    },
    {
      "code": 6050,
      "name": "approvalViaCpi",
      "msg": "Venue approvals can only be written by a top-level instruction"
    },
    {
      "code": 6051,
      "name": "invalidPoolParams",
      "msg": "Invalid LP pool parameters"
    },
    {
      "code": 6052,
      "name": "marketNotFresh",
      "msg": "Market or pool is not fresh"
    },
    {
      "code": 6053,
      "name": "poolExposureLimit",
      "msg": "Trade would take the pool's net exposure past its cap"
    },
    {
      "code": 6054,
      "name": "poolWeekendExposureLimit",
      "msg": "Trade would take the pool's net exposure past its weekend cap"
    },
    {
      "code": 6055,
      "name": "accountExposureLimit",
      "msg": "Trade would take this account's house notional past its cap"
    },
    {
      "code": 6056,
      "name": "poolAdlPending",
      "msg": "Pool has pending bad debt awaiting ADL"
    },
    {
      "code": 6057,
      "name": "poolCashInsufficient",
      "msg": "Pool cash cannot cover this payout yet"
    },
    {
      "code": 6058,
      "name": "poolWithdrawalsBlocked",
      "msg": "LP withdrawals are blocked by the pool drawdown"
    },
    {
      "code": 6059,
      "name": "poolCapWouldBreach",
      "msg": "Withdrawal would leave net exposure beyond the pool's hard line or weekend bound"
    },
    {
      "code": 6060,
      "name": "poolCashCapExceeded",
      "msg": "Deposit would exceed the pool's capital cap"
    },
    {
      "code": 6061,
      "name": "poolInsolvent",
      "msg": "Pool NAV is not positive"
    },
    {
      "code": 6062,
      "name": "poolHasNoLps",
      "msg": "Pool has no LP capital yet"
    },
    {
      "code": 6063,
      "name": "slippageExceeded",
      "msg": "Price or shares are worse than the acceptable limit"
    },
    {
      "code": 6064,
      "name": "invalidLpAmount",
      "msg": "Invalid LP amount"
    },
    {
      "code": 6065,
      "name": "invalidLpClass",
      "msg": "Invalid LP share class"
    },
    {
      "code": 6066,
      "name": "insufficientShares",
      "msg": "Not enough free LP shares"
    },
    {
      "code": 6067,
      "name": "withdrawalPending",
      "msg": "A withdrawal request is already pending"
    },
    {
      "code": 6068,
      "name": "noWithdrawalPending",
      "msg": "No withdrawal request is pending"
    },
    {
      "code": 6069,
      "name": "withdrawalDelayNotElapsed",
      "msg": "Withdrawal delay has not elapsed"
    },
    {
      "code": 6070,
      "name": "nothingToSweep",
      "msg": "No insurance excess to sweep"
    },
    {
      "code": 6071,
      "name": "insufficientTreasury",
      "msg": "Amount exceeds the treasury"
    },
    {
      "code": 6072,
      "name": "nothingToSettle",
      "msg": "Position has no unrealized loss to settle"
    },
    {
      "code": 6073,
      "name": "settleWouldBankrupt",
      "msg": "Settling would exhaust the position's collateral; liquidate instead"
    },
    {
      "code": 6074,
      "name": "invalidAdlMode",
      "msg": "Invalid ADL mode"
    },
    {
      "code": 6075,
      "name": "adlUnauthorized",
      "msg": "Only the ADL keeper or admin may run exposure ADL"
    },
    {
      "code": 6076,
      "name": "adlNotRequired",
      "msg": "ADL is not required"
    },
    {
      "code": 6077,
      "name": "adlWrongSide",
      "msg": "Position is on the wrong side for this ADL"
    },
    {
      "code": 6078,
      "name": "adlCandidateNotProfitable",
      "msg": "ADL candidate is not profitable"
    },
    {
      "code": 6079,
      "name": "adlScoreBelowFloor",
      "msg": "ADL candidate's score is below the floor"
    },
    {
      "code": 6080,
      "name": "adlOvershoot",
      "msg": "ADL would reduce net exposure past its target"
    },
    {
      "code": 6081,
      "name": "badDebtNotStale",
      "msg": "Pending bad debt is not old enough to write off"
    },
    {
      "code": 6082,
      "name": "fundingNotDue",
      "msg": "Funding is not due yet"
    },
    {
      "code": 6083,
      "name": "paramsTimelocked",
      "msg": "Pool parameter change is still timelocked"
    },
    {
      "code": 6084,
      "name": "noPendingParams",
      "msg": "No pool parameter change is pending"
    },
    {
      "code": 6085,
      "name": "bucketUnderflow",
      "msg": "Pool bucket would go negative"
    },
    {
      "code": 6086,
      "name": "vaultInvariantViolated",
      "msg": "Vault balance does not cover the pool buckets"
    },
    {
      "code": 6087,
      "name": "poolDrawdownFreeze",
      "msg": "Pool drawdown freezes net-raising opens"
    },
    {
      "code": 6088,
      "name": "lpFlowWindowClosed",
      "msg": "LP flows are closed in this window"
    },
    {
      "code": 6089,
      "name": "fundingStale",
      "msg": "Funding must be cranked first"
    },
    {
      "code": 6090,
      "name": "withdrawalExpired",
      "msg": "Withdrawal request has lapsed; cancel and request again"
    },
    {
      "code": 6091,
      "name": "marketNotSettled",
      "msg": "Market is not settled"
    },
    {
      "code": 6092,
      "name": "profitCloseQuiet",
      "msg": "Profitable closes wait profit_close_quiet_secs after a session open"
    },
    {
      "code": 6093,
      "name": "oracleNotFullyVerified",
      "msg": "Oracle price update is not fully verified"
    },
    {
      "code": 6094,
      "name": "oracleFeedIdMismatch",
      "msg": "Oracle price update is for another feed id"
    },
    {
      "code": 6095,
      "name": "noPendingOracleChange",
      "msg": "No oracle feed change is pending"
    },
    {
      "code": 6096,
      "name": "oracleChangeTimelocked",
      "msg": "Oracle feed change is still timelocked"
    }
  ],
  "types": [
    {
      "name": "adl",
      "docs": [
        "§7.2 step 11. `triggers` is a bit set of `pool_math::TRIGGER_*` (mode 1)."
      ],
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "mode",
            "type": "u8"
          },
          {
            "name": "position",
            "type": "pubkey"
          },
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "sizeClosed",
            "type": "u64"
          },
          {
            "name": "mark",
            "type": "u64"
          },
          {
            "name": "profit",
            "type": "u64"
          },
          {
            "name": "solvencyHc",
            "type": "u64"
          },
          {
            "name": "badDebtHc",
            "type": "u64"
          },
          {
            "name": "score",
            "type": "u128"
          },
          {
            "name": "floor",
            "type": "u64"
          },
          {
            "name": "triggers",
            "type": "u8"
          },
          {
            "name": "netBefore",
            "type": "i128"
          },
          {
            "name": "netAfter",
            "type": "i128"
          },
          {
            "name": "cashAfter",
            "type": "u64"
          },
          {
            "name": "b",
            "type": "u128"
          },
          {
            "name": "fee",
            "docs": [
              "Mode 0: the close fee collected from the slice (D22); mode 1: 0."
            ],
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "badDebt",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "position",
            "type": "pubkey"
          },
          {
            "name": "side",
            "type": "u8"
          },
          {
            "name": "deficit",
            "type": "u64"
          },
          {
            "name": "insurancePaid",
            "type": "u64"
          },
          {
            "name": "lpAbsorbed",
            "type": "u64"
          },
          {
            "name": "pendingAdded",
            "type": "u64"
          },
          {
            "name": "pendingLong",
            "type": "u64"
          },
          {
            "name": "pendingShort",
            "type": "u64"
          },
          {
            "name": "pendingSinceLong",
            "type": "i64"
          },
          {
            "name": "pendingSinceShort",
            "type": "i64"
          },
          {
            "name": "windowLpAbsorbed",
            "type": "u64"
          },
          {
            "name": "windowInsPaid",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "badDebtWrittenOff",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "amount",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "createReferralCodeParams",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "code",
            "docs": [
              "The referral code (will be uppercased, max 8 chars)"
            ],
            "type": {
              "array": [
                "u8",
                8
              ]
            }
          }
        ]
      }
    },
    {
      "name": "executionSource",
      "type": {
        "kind": "enum",
        "variants": [
          {
            "name": "orderBook"
          },
          {
            "name": "amm"
          },
          {
            "name": "venue"
          }
        ]
      }
    },
    {
      "name": "fundingUpdated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "periods",
            "type": "u64"
          },
          {
            "name": "periodsDropped",
            "type": "u64"
          },
          {
            "name": "rate",
            "type": "i64"
          },
          {
            "name": "step",
            "type": "i64"
          },
          {
            "name": "cumulativeFunding",
            "type": "i64"
          },
          {
            "name": "carrySecs",
            "type": "u32"
          }
        ]
      }
    },
    {
      "name": "initializeMarketParams",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "commodity",
            "type": "string"
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
            "name": "maxOpenInterest",
            "type": "u64"
          },
          {
            "name": "fundingInterval",
            "type": "i64"
          },
          {
            "name": "pythFeedId",
            "docs": [
              "Pyth feed id the `pyth_price_feed` account must carry (non-zero;",
              "docs/lp-pool.md D47)"
            ],
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          }
        ]
      }
    },
    {
      "name": "insuranceSwept",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "amount",
            "type": "u64"
          },
          {
            "name": "recovered",
            "type": "u64"
          },
          {
            "name": "vested",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "lpDeposited",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "amount",
            "type": "u64"
          },
          {
            "name": "shares",
            "type": "u64"
          },
          {
            "name": "navMint",
            "type": "i128"
          },
          {
            "name": "lpShares",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "lpPool",
      "docs": [
        "PDA `[b\"lp_pool\", market]`, one per market. Offsets (with the",
        "discriminator) in comments; pinned by `lp_pool_layout_is_pinned` and the",
        "`tests/fixtures/lp_pool.bin` fixture the keeper decodes."
      ],
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "adlKeeper",
            "type": "pubkey"
          },
          {
            "name": "freeCollateral",
            "type": "u64"
          },
          {
            "name": "positionCollateral",
            "type": "u64"
          },
          {
            "name": "cash",
            "type": "u64"
          },
          {
            "name": "treasury",
            "type": "u64"
          },
          {
            "name": "longEntrySum",
            "type": "u128"
          },
          {
            "name": "shortEntrySum",
            "type": "u128"
          },
          {
            "name": "longFundingSum",
            "type": "i128"
          },
          {
            "name": "shortFundingSum",
            "type": "i128"
          },
          {
            "name": "openPositions",
            "type": "u64"
          },
          {
            "name": "lpShares",
            "type": "u64"
          },
          {
            "name": "pendingWithdrawShares",
            "type": "u64"
          },
          {
            "name": "maxPoolCash",
            "type": "u64"
          },
          {
            "name": "pendingBadDebtLong",
            "type": "u64"
          },
          {
            "name": "pendingBadDebtShort",
            "type": "u64"
          },
          {
            "name": "pendingSinceLong",
            "type": "i64"
          },
          {
            "name": "badDebtCum",
            "type": "u64"
          },
          {
            "name": "badDebtRecoveredCum",
            "type": "u64"
          },
          {
            "name": "badDebtWrittenOffCum",
            "type": "u64"
          },
          {
            "name": "lossWindowStart",
            "type": "i64"
          },
          {
            "name": "lossWindowNav",
            "type": "u64"
          },
          {
            "name": "lpAbsorbedWindow",
            "type": "u64"
          },
          {
            "name": "insPaidWindow",
            "type": "u64"
          },
          {
            "name": "npsPeak",
            "type": "u64"
          },
          {
            "name": "npsPeakAt",
            "type": "i64"
          },
          {
            "name": "vestAmount",
            "type": "u64"
          },
          {
            "name": "vestStart",
            "type": "i64"
          },
          {
            "name": "vestEnd",
            "type": "i64"
          },
          {
            "name": "donatedCum",
            "type": "u64"
          },
          {
            "name": "pendingParamsAt",
            "type": "i64"
          },
          {
            "name": "params",
            "type": {
              "defined": {
                "name": "poolParams"
              }
            }
          },
          {
            "name": "pendingParams",
            "type": {
              "defined": {
                "name": "poolParams"
              }
            }
          },
          {
            "name": "bump",
            "type": "u8"
          },
          {
            "name": "pendingSinceShort",
            "type": "i64"
          }
        ]
      }
    },
    {
      "name": "lpPosition",
      "docs": [
        "PDA `[b\"lp_position\", lp_pool, owner, &[class]]`."
      ],
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "pool",
            "type": "pubkey"
          },
          {
            "name": "shares",
            "type": "u64"
          },
          {
            "name": "pendingShares",
            "type": "u64"
          },
          {
            "name": "requestedValue",
            "type": "u64"
          },
          {
            "name": "requestedAt",
            "type": "i64"
          },
          {
            "name": "unlockAt",
            "type": "i64"
          },
          {
            "name": "depositedCum",
            "type": "u64"
          },
          {
            "name": "withdrawnCum",
            "type": "u64"
          },
          {
            "name": "claimWindowSecs",
            "type": "u32"
          },
          {
            "name": "class",
            "type": "u8"
          },
          {
            "name": "bump",
            "type": "u8"
          }
        ]
      }
    },
    {
      "name": "lpWithdrawalCancelled",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "shares",
            "type": "u64"
          },
          {
            "name": "lpShares",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "lpWithdrawalRequested",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "shares",
            "type": "u64"
          },
          {
            "name": "value",
            "docs": [
              "`requested_value`"
            ],
            "type": "u64"
          },
          {
            "name": "navRedeem",
            "type": "i128"
          },
          {
            "name": "lpShares",
            "type": "u64"
          },
          {
            "name": "unlockAt",
            "type": "i64"
          }
        ]
      }
    },
    {
      "name": "lpWithdrawn",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "shares",
            "type": "u64"
          },
          {
            "name": "value",
            "type": "u64"
          },
          {
            "name": "navRedeem",
            "type": "i128"
          },
          {
            "name": "lpShares",
            "type": "u64"
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
      "name": "marketConfigParams",
      "docs": [
        "Authority-controlled risk settings. See the field docs on `Market`."
      ],
      "type": {
        "kind": "struct",
        "fields": [
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
            "name": "maxOpenInterest",
            "docs": [
              "Per-side open-interest cap in contracts (D6). `0` keeps the current",
              "value, so `Default`-based callers are unchanged (red-team F19). It may be",
              "set below the current OI, and then only blocks increases."
            ],
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "openPositionParams",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "side",
            "type": "u8"
          },
          {
            "name": "size",
            "type": "u64"
          },
          {
            "name": "leverage",
            "type": "u32"
          },
          {
            "name": "acceptablePrice",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "oracleFeedApplied",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "priceFeed",
            "type": "pubkey"
          },
          {
            "name": "feedId",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "effectiveAt",
            "type": "i64"
          }
        ]
      }
    },
    {
      "name": "oracleFeedCancelled",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "priceFeed",
            "type": "pubkey"
          },
          {
            "name": "feedId",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "effectiveAt",
            "type": "i64"
          }
        ]
      }
    },
    {
      "name": "oracleFeedProposed",
      "docs": [
        "Oracle feed rotation (docs/lp-pool.md D47). `price_feed` / `feed_id` are",
        "the proposed (Proposed, Cancelled) or new (Applied) feed; `effective_at`",
        "is when it may be or was applied."
      ],
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "priceFeed",
            "type": "pubkey"
          },
          {
            "name": "feedId",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "effectiveAt",
            "type": "i64"
          }
        ]
      }
    },
    {
      "name": "poolDonated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "donor",
            "type": "pubkey"
          },
          {
            "name": "amount",
            "type": "u64"
          },
          {
            "name": "recovered",
            "type": "u64"
          },
          {
            "name": "vested",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "poolParams",
      "docs": [
        "Pool risk and fee parameters (§1.2). Borsh, 104 bytes; offsets in comments."
      ],
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "skewScale",
            "type": "u64"
          },
          {
            "name": "insuranceTarget",
            "type": "u64"
          },
          {
            "name": "insuranceClaimCapPerWindow",
            "type": "u64"
          },
          {
            "name": "liqMinRemainingNotional",
            "type": "u64"
          },
          {
            "name": "kNetBps",
            "type": "u32"
          },
          {
            "name": "hardNetBps",
            "type": "u32"
          },
          {
            "name": "perAccountCapBps",
            "type": "u32"
          },
          {
            "name": "weekendLpLossBps",
            "type": "u32"
          },
          {
            "name": "weekendGapBps",
            "type": "u32"
          },
          {
            "name": "weekendCapWindowSecs",
            "type": "u32"
          },
          {
            "name": "withdrawalDelaySecs",
            "type": "u32"
          },
          {
            "name": "withdrawalClaimWindowSecs",
            "type": "u32"
          },
          {
            "name": "lpReopenQuietSecs",
            "type": "u32"
          },
          {
            "name": "adlNetTargetBps",
            "type": "u32"
          },
          {
            "name": "adlExposureScoreFloorBps",
            "type": "u32"
          },
          {
            "name": "ddFreezeBps",
            "type": "u16"
          },
          {
            "name": "ddAdlBps",
            "type": "u16"
          },
          {
            "name": "ddWithdrawBlockBps",
            "type": "u16"
          },
          {
            "name": "redeemLossHaircutBps",
            "type": "u16"
          },
          {
            "name": "openFeeBps",
            "type": "u16"
          },
          {
            "name": "maxImpactBps",
            "type": "u16"
          },
          {
            "name": "liqPenaltyBps",
            "type": "u16"
          },
          {
            "name": "liqKeeperShareBps",
            "type": "u16"
          },
          {
            "name": "backstopThresholdBps",
            "type": "u16"
          },
          {
            "name": "minLiqSliceBps",
            "type": "u16"
          },
          {
            "name": "treasuryFeeBps",
            "type": "u16"
          },
          {
            "name": "lpLossCapWindowBps",
            "type": "u16"
          },
          {
            "name": "profitCloseQuietSecs",
            "type": "u32"
          }
        ]
      }
    },
    {
      "name": "poolParamsApplied",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "params",
            "type": {
              "defined": {
                "name": "poolParams"
              }
            }
          },
          {
            "name": "effectiveAt",
            "type": "i64"
          }
        ]
      }
    },
    {
      "name": "poolParamsCancelled",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "params",
            "type": {
              "defined": {
                "name": "poolParams"
              }
            }
          },
          {
            "name": "effectiveAt",
            "type": "i64"
          }
        ]
      }
    },
    {
      "name": "poolParamsProposed",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "params",
            "type": {
              "defined": {
                "name": "poolParams"
              }
            }
          },
          {
            "name": "effectiveAt",
            "type": "i64"
          }
        ]
      }
    },
    {
      "name": "position",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "side",
            "type": {
              "defined": {
                "name": "side"
              }
            }
          },
          {
            "name": "size",
            "type": "u64"
          },
          {
            "name": "collateral",
            "type": "u64"
          },
          {
            "name": "entryPrice",
            "type": "u64"
          },
          {
            "name": "leverage",
            "type": "u32"
          },
          {
            "name": "realizedPnl",
            "type": "i64"
          },
          {
            "name": "lastFundingPayment",
            "type": "i64"
          },
          {
            "name": "openedAt",
            "type": "i64"
          },
          {
            "name": "lastUpdatedAt",
            "type": "i64"
          },
          {
            "name": "status",
            "type": {
              "defined": {
                "name": "positionStatus"
              }
            }
          },
          {
            "name": "executionSource",
            "type": {
              "defined": {
                "name": "executionSource"
              }
            }
          },
          {
            "name": "bump",
            "type": "u8"
          },
          {
            "name": "openedPrice",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "positionClosed",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "position",
            "type": "pubkey"
          },
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "mark",
            "type": "u64"
          },
          {
            "name": "execPrice",
            "type": "u64"
          },
          {
            "name": "pnl",
            "type": "i64"
          },
          {
            "name": "funding",
            "type": "i64"
          },
          {
            "name": "solvencyHc",
            "type": "u64"
          },
          {
            "name": "badDebtHc",
            "type": "u64"
          },
          {
            "name": "fee",
            "type": "u64"
          },
          {
            "name": "referralPaid",
            "type": "u64"
          },
          {
            "name": "payout",
            "type": "u64"
          },
          {
            "name": "deficit",
            "type": "u64"
          },
          {
            "name": "settledClose",
            "type": "bool"
          }
        ]
      }
    },
    {
      "name": "positionLiquidated",
      "docs": [
        "§6.5. `stage` is 1 (partial / full with leftover to the owner) or 2",
        "(backstop). `insurance_delta` is the signed change of the insurance fund."
      ],
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "position",
            "type": "pubkey"
          },
          {
            "name": "stage",
            "type": "u8"
          },
          {
            "name": "sizeClosed",
            "type": "u64"
          },
          {
            "name": "sizeRemaining",
            "type": "u64"
          },
          {
            "name": "mark",
            "type": "u64"
          },
          {
            "name": "penalty",
            "type": "u64"
          },
          {
            "name": "keeperReward",
            "type": "u64"
          },
          {
            "name": "ownerLeftover",
            "type": "u64"
          },
          {
            "name": "insuranceDelta",
            "type": "i64"
          },
          {
            "name": "deficit",
            "type": "u64"
          },
          {
            "name": "solvencyHc",
            "type": "u64"
          },
          {
            "name": "badDebtHc",
            "type": "u64"
          },
          {
            "name": "cashHc",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "positionOpened",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "position",
            "type": "pubkey"
          },
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "side",
            "type": "u8"
          },
          {
            "name": "size",
            "type": "u64"
          },
          {
            "name": "oracle",
            "type": "u64"
          },
          {
            "name": "execPrice",
            "type": "u64"
          },
          {
            "name": "collateral",
            "type": "u64"
          },
          {
            "name": "openFee",
            "type": "u64"
          },
          {
            "name": "netAfter",
            "type": "i128"
          },
          {
            "name": "navRedeem",
            "type": "i128"
          }
        ]
      }
    },
    {
      "name": "positionPnlSettled",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "position",
            "type": "pubkey"
          },
          {
            "name": "mark",
            "type": "u64"
          },
          {
            "name": "loss",
            "type": "u64"
          },
          {
            "name": "newCollateral",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "positionStatus",
      "type": {
        "kind": "enum",
        "variants": [
          {
            "name": "open"
          },
          {
            "name": "closed"
          },
          {
            "name": "liquidated"
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
      "name": "protocolRolesParams",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "guardian",
            "type": "pubkey"
          },
          {
            "name": "oracleProgram",
            "type": "pubkey"
          }
        ]
      }
    },
    {
      "name": "referralCode",
      "docs": [
        "On-chain referral code account",
        "PDA seeds: [b\"referral_code\", code_bytes]"
      ],
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "owner",
            "docs": [
              "The wallet that owns this referral code"
            ],
            "type": "pubkey"
          },
          {
            "name": "code",
            "docs": [
              "The referral code (8 bytes, uppercase alphanumeric)"
            ],
            "type": {
              "array": [
                "u8",
                8
              ]
            }
          },
          {
            "name": "discountBps",
            "docs": [
              "Discount percent for referred users (basis points, 1000 = 10%)"
            ],
            "type": "u16"
          },
          {
            "name": "rewardBps",
            "docs": [
              "Reward percent for referrer (basis points, 2000 = 20%)"
            ],
            "type": "u16"
          },
          {
            "name": "totalReferred",
            "docs": [
              "Total number of users referred"
            ],
            "type": "u32"
          },
          {
            "name": "totalVolume",
            "docs": [
              "Total trading volume from referrals (6 decimals)"
            ],
            "type": "u64"
          },
          {
            "name": "totalFeesGenerated",
            "docs": [
              "Total fees generated by referrals (6 decimals)"
            ],
            "type": "u64"
          },
          {
            "name": "totalRewardsEarned",
            "docs": [
              "Total rewards earned (6 decimals)"
            ],
            "type": "u64"
          },
          {
            "name": "pendingRewards",
            "docs": [
              "Pending rewards available to claim (6 decimals)"
            ],
            "type": "u64"
          },
          {
            "name": "createdAt",
            "docs": [
              "Timestamp when the code was created"
            ],
            "type": "i64"
          },
          {
            "name": "isActive",
            "docs": [
              "Whether this code is active"
            ],
            "type": "bool"
          },
          {
            "name": "bump",
            "docs": [
              "PDA bump"
            ],
            "type": "u8"
          }
        ]
      }
    },
    {
      "name": "side",
      "type": {
        "kind": "enum",
        "variants": [
          {
            "name": "long"
          },
          {
            "name": "short"
          }
        ]
      }
    },
    {
      "name": "treasuryWithdrawn",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "amount",
            "type": "u64"
          },
          {
            "name": "destination",
            "type": "pubkey"
          }
        ]
      }
    },
    {
      "name": "userAccount",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "collateralBalance",
            "type": "u64"
          },
          {
            "name": "totalPositions",
            "type": "u32"
          },
          {
            "name": "totalTrades",
            "type": "u64"
          },
          {
            "name": "realizedPnl",
            "type": "i64"
          },
          {
            "name": "bump",
            "type": "u8"
          },
          {
            "name": "houseEntryNotional",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "userReferral",
      "docs": [
        "User's referral relationship - tracks which code a user applied",
        "PDA seeds: [b\"user_referral\", user_pubkey]"
      ],
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "user",
            "docs": [
              "The user who applied a referral code"
            ],
            "type": "pubkey"
          },
          {
            "name": "referralCode",
            "docs": [
              "The referral code they're using"
            ],
            "type": "pubkey"
          },
          {
            "name": "referrer",
            "docs": [
              "The referrer's wallet (for quick lookup)"
            ],
            "type": "pubkey"
          },
          {
            "name": "discountBps",
            "docs": [
              "Discount this user receives (cached from referral code)"
            ],
            "type": "u16"
          },
          {
            "name": "totalVolume",
            "docs": [
              "Total volume traded by this user"
            ],
            "type": "u64"
          },
          {
            "name": "totalFeesPaid",
            "docs": [
              "Total fees paid by this user"
            ],
            "type": "u64"
          },
          {
            "name": "totalReferrerRewards",
            "docs": [
              "Total fees sent to referrer"
            ],
            "type": "u64"
          },
          {
            "name": "appliedAt",
            "docs": [
              "When the referral was applied"
            ],
            "type": "i64"
          },
          {
            "name": "bump",
            "docs": [
              "PDA bump"
            ],
            "type": "u8"
          }
        ]
      }
    },
    {
      "name": "vault",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "collateralMint",
            "type": "pubkey"
          },
          {
            "name": "tokenAccount",
            "type": "pubkey"
          },
          {
            "name": "totalDeposits",
            "type": "u64"
          },
          {
            "name": "bump",
            "type": "u8"
          }
        ]
      }
    },
    {
      "name": "venue",
      "docs": [
        "A program allowed to open matched positions in one market with venue_fill.",
        "",
        "Only that program can sign for `authority` (its PDA `[b\"perps_venue\",",
        "market]`), so the record binds the program, the market and the signer",
        "together. `upgrade_authority` pins who can change the venue's code: if the",
        "venue's ProgramData names any other upgrade authority, fills halt until",
        "the admin re-attests with update_venue. (Pinning the deploy slot instead",
        "is griefable: loader ExtendProgram needs no authority and rewrites",
        "ProgramData.slot, so anyone could halt every venue for a few lamports.)"
      ],
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "program",
            "type": "pubkey"
          },
          {
            "name": "authority",
            "type": "pubkey"
          },
          {
            "name": "programData",
            "type": "pubkey"
          },
          {
            "name": "maxFillNotional",
            "type": "u64"
          },
          {
            "name": "totalFills",
            "type": "u64"
          },
          {
            "name": "totalNotional",
            "type": "u64"
          },
          {
            "name": "maxPriceDeviationBps",
            "type": "u16"
          },
          {
            "name": "maxOracleAgeSecs",
            "type": "u16"
          },
          {
            "name": "minSigners",
            "type": "u8"
          },
          {
            "name": "enabled",
            "type": "bool"
          },
          {
            "name": "bump",
            "type": "u8"
          },
          {
            "name": "upgradeAuthority",
            "type": "pubkey"
          }
        ]
      }
    },
    {
      "name": "venueApproval",
      "docs": [
        "One owner's consent to fills through one venue in one role.",
        "",
        "Perps-core signs and enforces these bounds itself, so a buggy or hostile",
        "venue can never commit more of the owner's collateral, at worse prices or",
        "higher leverage, than the owner agreed to here. Taker and maker consent are",
        "separate accounts: an MM's standing maker approval must not let a venue",
        "make it the taker of someone else's order."
      ],
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "venue",
            "type": "pubkey"
          },
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "allowance",
            "type": "u64"
          },
          {
            "name": "maxBuyPrice",
            "type": "u64"
          },
          {
            "name": "minSellPrice",
            "type": "u64"
          },
          {
            "name": "expiresAt",
            "type": "i64"
          },
          {
            "name": "fills",
            "type": "u64"
          },
          {
            "name": "collateralCommitted",
            "type": "u64"
          },
          {
            "name": "maxLeverage",
            "type": "u32"
          },
          {
            "name": "maxPriceDeviationBps",
            "type": "u16"
          },
          {
            "name": "bump",
            "type": "u8"
          },
          {
            "name": "role",
            "type": "u8"
          },
          {
            "name": "allowedSides",
            "type": "u8"
          }
        ]
      }
    },
    {
      "name": "venueApprovalParams",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "allowance",
            "type": "u64"
          },
          {
            "name": "maxLeverage",
            "type": "u32"
          },
          {
            "name": "maxBuyPrice",
            "type": "u64"
          },
          {
            "name": "minSellPrice",
            "type": "u64"
          },
          {
            "name": "maxPriceDeviationBps",
            "type": "u16"
          },
          {
            "name": "expiresAt",
            "type": "i64"
          },
          {
            "name": "allowedSides",
            "type": "u8"
          }
        ]
      }
    },
    {
      "name": "venueFillParams",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "size",
            "type": "u64"
          },
          {
            "name": "price",
            "type": "u64"
          },
          {
            "name": "takerSide",
            "type": "u8"
          },
          {
            "name": "takerLeverage",
            "type": "u32"
          },
          {
            "name": "makerLeverage",
            "type": "u32"
          },
          {
            "name": "positionNonce",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "venueFilled",
      "docs": [
        "Positions are one per fill, so this is the trade tape."
      ],
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "venue",
            "type": "pubkey"
          },
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "taker",
            "type": "pubkey"
          },
          {
            "name": "maker",
            "type": "pubkey"
          },
          {
            "name": "takerPosition",
            "type": "pubkey"
          },
          {
            "name": "makerPosition",
            "type": "pubkey"
          },
          {
            "name": "takerSide",
            "type": "u8"
          },
          {
            "name": "size",
            "type": "u64"
          },
          {
            "name": "price",
            "type": "u64"
          },
          {
            "name": "oraclePrice",
            "type": "u64"
          },
          {
            "name": "takerCollateral",
            "type": "u64"
          },
          {
            "name": "makerCollateral",
            "type": "u64"
          },
          {
            "name": "timestamp",
            "type": "i64"
          }
        ]
      }
    },
    {
      "name": "venueParams",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "maxPriceDeviationBps",
            "type": "u16"
          },
          {
            "name": "maxOracleAgeSecs",
            "type": "u16"
          },
          {
            "name": "minSigners",
            "type": "u8"
          },
          {
            "name": "maxFillNotional",
            "type": "u64"
          }
        ]
      }
    }
  ]
};
