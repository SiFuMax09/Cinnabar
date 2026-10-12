import java.io.PrintWriter;
import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Direction;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.EmptyBlockGetter;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;

public class Probe {
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        try (PrintWriter out = new PrintWriter(args[0])) {
            for (BlockState s : Block.BLOCK_STATE_REGISTRY) {
                int id = Block.getId(s);
                boolean solid = s.isRedstoneConductor(EmptyBlockGetter.INSTANCE, BlockPos.ZERO);
                boolean cube = s.isFaceSturdy(EmptyBlockGetter.INSTANCE, BlockPos.ZERO, Direction.UP);
                boolean full = s.isCollisionShapeFullBlock(EmptyBlockGetter.INSTANCE, BlockPos.ZERO);
                out.println(id + " " + (solid ? 1 : 0) + " " + (cube ? 1 : 0) + " " + (full ? 1 : 0) + " " + s.instrument().getSerializedName());
            }
        }
    }
}
