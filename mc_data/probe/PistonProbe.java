import java.io.PrintWriter;
import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.EmptyBlockGetter;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;

/** Dumps what piston movement depends on: `<state id> <push reaction> <indestructible> <block entity>` */
public class PistonProbe {
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        try (PrintWriter out = new PrintWriter(args[0])) {
            for (BlockState s : Block.BLOCK_STATE_REGISTRY) {
                boolean indestructible = s.getDestroySpeed(EmptyBlockGetter.INSTANCE, BlockPos.ZERO) == -1.0F;
                out.println(Block.getId(s) + " " + s.getPistonPushReaction().name() + " " + (indestructible ? 1 : 0) + " " + (s.hasBlockEntity() ? 1 : 0));
            }
        }
    }
}
