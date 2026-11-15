# Import into the managing flake's outputs:
# rig = import ./rig-inventory.nix;
# Adapt names and paths to the configurations that this flake actually owns.
{
  version = 1;
  hosts = {
    hyperion = {
      platform = "darwin";
      managed = true;
      liveCheckout = "/Users/admin/src/system";
    };
    amarna = {
      platform = "nixos";
      managed = true;
      ssh = {
        host = "amarna";
        user = "sol";
        port = 22;
      };
      liveCheckout = "/home/sol/src/system";
      services.qwen-256k = {
        manager = "systemd";
        scope = "system";
        unit = "qwen-256k.service";
        boot = false;
        timeout = 60;
        # Add an absolute argv readiness probe appropriate to this service.
        # Without one Rig reports native running state, not API readiness.
      };
    };
    atlas = {
      platform = "darwin";
      managed = false;
      owner = "the private work flake";
    };
  };
  # Optional metadata only; agenix remains the source of recipient policy.
  # Generate this from the same recipient rules instead of maintaining a copy.
  secrets = { };
}
