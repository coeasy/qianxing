#include <qianxing_ring.hpp>
#include <qianxing_strategy.h>

int main() {
    return QX_STRATEGY_API_VERSION == 1u ? 0 : 1;
}
