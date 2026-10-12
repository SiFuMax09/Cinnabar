import java.io.PrintWriter;
import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Direction;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.EmptyBlockGetter;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.lighting.LightEngine;
import net.minecraft.world.phys.shapes.Shapes;

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
                net.minecraft.world.phys.shapes.VoxelShape shape = s.getCollisionShape(EmptyBlockGetter.INSTANCE, BlockPos.ZERO);
                String bounds = "none";
                if (!shape.isEmpty()) {
                    net.minecraft.world.phys.AABB box = shape.bounds();
                    bounds = Math.round(box.minX * 16) + "," + Math.round(box.minY * 16) + "," + Math.round(box.minZ * 16) + ","
                        + Math.round(box.maxX * 16) + "," + Math.round(box.maxY * 16) + "," + Math.round(box.maxZ * 16);
                }
                // Bit n is set when the occlusion shape of the face in direction n (in `Direction`
                // order: down, up, north, south, west, east) covers the whole face
                int faces = 0;
                for (Direction dir : Direction.values()) {
                    if (Shapes.faceShapeOccludes(LightEngine.getOcclusionShape(s, dir), Shapes.empty())) {
                        faces |= 1 << dir.ordinal();
                    }
                }
                out.println(id + " " + (solid ? 1 : 0) + " " + (cube ? 1 : 0) + " " + (full ? 1 : 0) + " " + s.instrument().getSerializedName() + " " + bounds + " " + s.getBlock().getExplosionResistance()
                    + " " + s.getLightEmission() + " " + s.getLightDampening() + " " + faces);
            }
        }
    }
}
