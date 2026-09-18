{
  description = "Logos uniswap_backend — the Uniswap app's backend, composing reusable EVM modules.";

  inputs = {
    logos-module-builder.url = "github:logos-co/logos-module-builder";
    # One node per module: every diamond follows the top-level input, so the lock stays small
    # and each dependency's generated client comes from one contract.
    eth_rpc_module = {
      url = "github:logos-co/logos-evm-eth-rpc-module";
      inputs.logos-module-builder.follows = "logos-module-builder";
    };
    token_list_module = {
      url = "github:logos-co/logos-evm-token-list-module";
      inputs.logos-module-builder.follows = "logos-module-builder";
    };
    evm_assets_module = {
      url = "github:logos-co/logos-evm-assets-module";
      inputs.logos-module-builder.follows = "logos-module-builder";
      inputs.eth_rpc_module.follows = "eth_rpc_module";
    };
    fee_module = {
      url = "github:logos-co/logos-evm-fee-module";
      inputs.logos-module-builder.follows = "logos-module-builder";
      inputs.eth_rpc_module.follows = "eth_rpc_module";
    };
    keystore_module = {
      url = "github:logos-co/logos-evm-keystore-module";
      inputs.logos-module-builder.follows = "logos-module-builder";
    };
    tx_sender_module = {
      url = "github:logos-co/logos-evm-tx-sender-module";
      inputs.logos-module-builder.follows = "logos-module-builder";
      inputs.eth_rpc_module.follows = "eth_rpc_module";
      inputs.fee_module.follows = "fee_module";
      inputs.keystore_module.follows = "keystore_module";
    };
    uniswap_module = {
      url = "github:logos-co/logos-evm-uniswap-module";
      inputs.logos-module-builder.follows = "logos-module-builder";
      inputs.eth_rpc_module.follows = "eth_rpc_module";
    };
  };

  outputs = inputs@{ self, logos-module-builder, ... }:
    let
      nixpkgs = logos-module-builder.inputs.nixpkgs;
      systems = [ "aarch64-darwin" "x86_64-darwin" "aarch64-linux" "x86_64-linux" ];

      # x86_64-windows is a cross PSEUDO-SYSTEM the builder already understands (it routes it to
      # logos-nix's mkWindowsPkgs). A target, never a host evaluated natively, so only `packages`.
      targets = systems ++ [ "x86_64-windows" ];
      forAllTargets = f: nixpkgs.lib.genAttrs targets f;
    in
    {
      packages = forAllTargets (system:
        (logos-module-builder.lib.mkLogosModule {
          src = ./.;
          configFile = ./metadata.json;
          flakeInputs = inputs;
        }).packages.${system});
    };
}
