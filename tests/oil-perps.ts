import * as anchor from "@project-serum/anchor";
import { Program } from "@project-serum/anchor";
import { OilPerps } from "../target/types/oil_perps";

describe("oil-perps", () => {
  // Configure the client to use the local cluster.
  anchor.setProvider(anchor.AnchorProvider.env());

  const program = anchor.workspace.OilPerps as Program<OilPerps>;

  it("Is initialized!", async () => {
    // Add your test here.
    const tx = await program.methods.initialize().rpc();
    console.log("Your transaction signature", tx);
  });
});
