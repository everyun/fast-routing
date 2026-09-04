import app.freerouting.core.RoutingJob;
import app.freerouting.io.BoardReadResult;
import app.freerouting.io.specctra.DsnWriter;
import app.freerouting.management.HeadlessBoardManager;
import app.freerouting.settings.sources.DefaultSettings;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;

/** The G1.0 oracle dump: Java's resolved internal board model, re-serialised.
 *
 *  The release jar's headless `-de f.dsn -do f.import.dsn` pipeline cannot
 *  produce this — HeadlessBoardManager only ever writes SES. This tool walks
 *  the same load path instead: HeadlessBoardManager.loadFromSpecctraDsn, which
 *  is DsnReader plus the post-load steps the router's import takes
 *  (copper_to_edge / hole_edge clearance-class synthesis,
 *  reduce_nets_of_route_items), then DsnWriter. The pcb name follows the `-do
 *  f.import.dsn` convention: the output file's stem. */
public class DumpDsn {
  public static void main(String[] args) throws Exception {
    if (args.length != 2) {
      System.err.println("usage: DumpDsn <in.dsn> <out.import.dsn>");
      System.exit(2);
    }
    String outName = new java.io.File(args[1]).getName();
    String designName =
        outName.endsWith(".dsn") ? outName.substring(0, outName.length() - 4) : outName;
    RoutingJob job = new RoutingJob();
    // DefaultSettings.arm(): the CLI flow's clearance overrides — a bare
    // `new RouterSettings()` leaves both null and the post-load synthesis
    // (board_edge / hole_edge classes) never runs.
    job.routerSettings.copperToEdgeClearanceUm = DefaultSettings.DEFAULT_COPPER_TO_EDGE_CLEARANCE_UM;
    job.routerSettings.holeClearanceUm = DefaultSettings.DEFAULT_HOLE_CLEARANCE_UM;
    HeadlessBoardManager manager = new HeadlessBoardManager(job);
    BoardReadResult result;
    try (InputStream in = new FileInputStream(args[0])) {
      result = manager.loadFromSpecctraDsn(in, null, null);
    }
    if (manager.get_routing_board() == null) {
      System.err.println("read failed: " + result);
      System.exit(1);
      return;
    }
    try (OutputStream out = new FileOutputStream(args[1])) {
      DsnWriter.write(manager.get_routing_board(), out, designName, false);
    }
  }
}
