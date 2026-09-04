import app.freerouting.board.BasicBoard;
import app.freerouting.board.RoutingBoard;
import app.freerouting.io.BoardReadResult;
import app.freerouting.io.specctra.DsnReader;
import app.freerouting.io.specctra.DsnWriter;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;

/** Load a DSN through the release jar's resolved-model path and re-serialise it
 *  (the headless `-do` pipeline in v2.3.0 only writes SES, so the G1.0 oracle
 *  dump is produced here: DsnReader + reduce_nets_of_route_items + DsnWriter). */
public class DumpDsn {
  public static void main(String[] args) throws Exception {
    if (args.length != 2) {
      System.err.println("usage: DumpDsn <in.dsn> <out.dsn>");
      System.exit(2);
    }
    String designName = new java.io.File(args[0]).getName();
    BoardReadResult result;
    try (InputStream in = new FileInputStream(args[0])) {
      result = DsnReader.readBoard(in, null, null, designName);
    }
    BasicBoard board;
    if (result instanceof BoardReadResult.Success success) {
      board = success.board();
    } else if (result instanceof BoardReadResult.OutlineMissing missing) {
      board = missing.board();
    } else {
      System.err.println("read failed: " + result);
      System.exit(1);
      return;
    }
    if (board instanceof RoutingBoard routingBoard) {
      routingBoard.reduce_nets_of_route_items();
    }
    try (OutputStream out = new FileOutputStream(args[1])) {
      DsnWriter.write(board, out, designName, false);
    }
  }
}
